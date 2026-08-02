package ui

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// Palette holds the semantic colors of the built-in theme.
type Palette struct {
	Border      string
	Accent      string
	Muted       string
	Primary     string
	Selected    string
	Description string
	SelectedBG  string
	Ghost       string
}

// DefaultPalette is the built-in semantic palette.
var DefaultPalette = Palette{
	Border:      "#a277ff",
	Accent:      "#61ffca",
	Muted:       "#6d6a7f",
	Primary:     "#edecee",
	Selected:    "#ffffff",
	Description: "#9692a8",
	SelectedBG:  "#3d375e",
	Ghost:       "#4b4a4c",
}

// fg returns a truecolor foreground sequence for a #rrggbb color.
func fg(hex string) string {
	r, g, b, ok := splitHex(hex)
	if !ok {
		return ""
	}
	return fmt.Sprintf("\x1b[38;2;%d;%d;%dm", r, g, b)
}

// bg returns a truecolor background sequence for a #rrggbb color.
func bg(hex string) string {
	r, g, b, ok := splitHex(hex)
	if !ok {
		return ""
	}
	return fmt.Sprintf("\x1b[48;2;%d;%d;%dm", r, g, b)
}

const reset = "\x1b[0m"

func splitHex(hex string) (r, g, b int64, ok bool) {
	hex = strings.TrimPrefix(hex, "#")
	if len(hex) != 6 {
		return 0, 0, 0, false
	}
	var err error
	if r, err = strconv.ParseInt(hex[0:2], 16, 0); err != nil {
		return 0, 0, 0, false
	}
	if g, err = strconv.ParseInt(hex[2:4], 16, 0); err != nil {
		return 0, 0, 0, false
	}
	if b, err = strconv.ParseInt(hex[4:6], 16, 0); err != nil {
		return 0, 0, 0, false
	}
	return r, g, b, true
}

// nerdIcons maps icon keys to Nerd Font glyphs.
var nerdIcons = map[string]string{
	"git":        "",
	"go":         "",
	"rust":       "",
	"node":       "",
	"python":     "",
	"docker":     "",
	"kubernetes": "",
	"folder":     "",
	"file":       "",
	"ssh":        "",
	"env":        "",
	"process":    "",
	"package":    "",
	"task":       "",
	"shield":     "",
	"history":    "",
	"alias":      "",
	"ai":         "",
}

// neutralIcon is the fallback glyph for unknown commands.
const neutralIcon = ""

// icon resolves the glyph for a candidate; empty when Nerd Fonts are off.
func icon(c complete.Candidate, nerdFonts bool) string {
	if !nerdFonts {
		return ""
	}
	key := c.Icon
	if key == "" {
		switch c.Source {
		case complete.SourceHistory:
			key = "history"
		case complete.SourceAlias:
			key = "alias"
		case complete.SourceAI:
			key = "ai"
		}
	}
	if g, ok := nerdIcons[key]; ok {
		return g
	}
	return neutralIcon
}

// sourceLabel returns the pill text distinguishing candidate sources.
func sourceLabel(s complete.Source) string {
	switch s {
	case complete.SourceAlias:
		return "alias"
	case complete.SourceHistory:
		return "history"
	case complete.SourceSystem:
		return "system"
	case complete.SourceInferred:
		return "inferred"
	case complete.SourceAI:
		return "ai"
	default:
		return ""
	}
}
