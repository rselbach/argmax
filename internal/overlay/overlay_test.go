package overlay

import (
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func TestIconFor(t *testing.T) {
	canonical := []string{
		"git", "github", "docker", "kubernetes", "cloud", "database", "node",
		"python", "rust", "go", "java", "c", "build", "package", "fs",
		"archive", "editor", "viewer", "text", "json", "task", "sysadmin",
		"network", "process", "shell", "search", "vcs", "ai", "alias",
		"history", "system", "file", "directory", "misc",
	}
	for _, key := range canonical {
		g := IconFor(key, true)
		if g == "" {
			t.Errorf("IconFor(%q, true) is empty", key)
			continue
		}
		for _, r := range g {
			pua := (r >= 0xe000 && r <= 0xf8ff) || (r >= 0xf0000 && r <= 0xffffd)
			if !pua {
				t.Errorf("IconFor(%q) = %U, not a Private Use Area glyph", key, r)
			}
		}
	}

	if got := IconFor("git", false); got != "" {
		t.Errorf("IconFor(git, false) = %q, want empty (UI-010)", got)
	}
	if got := IconFor("no-such-key", true); got != IconFor("misc", true) {
		t.Errorf("IconFor(unknown) = %q, want misc fallback %q", got, IconFor("misc", true))
	}
	if IconFor("GIT", true) != IconFor("git", true) {
		t.Error("IconFor should be case-insensitive")
	}
}

func TestSourceLabel(t *testing.T) {
	cases := map[core.Source]string{
		core.SourceSpec:      "spec",
		core.SourceAlias:     "alias",
		core.SourceToolAlias: "alias",
		core.SourceSystem:    "system",
		core.SourceHistory:   "history",
		core.SourceInferred:  "inferred",
		core.SourceAI:        "ai",
		core.SourceFile:      "file",
		core.SourceDynamic:   "live",
	}
	for src, want := range cases {
		if got := SourceLabel(src); got != want {
			t.Errorf("SourceLabel(%q) = %q, want %q", src, got, want)
		}
	}
	if got := SourceLabel(core.Source("weird")); got != "weird" {
		t.Errorf("SourceLabel(unknown) = %q, want raw source name", got)
	}
}

func TestDefaultPalette(t *testing.T) {
	p := DefaultPalette()
	want := Palette{
		Border:      "\x1b[38;2;162;119;255m",
		Accent:      "\x1b[38;2;97;255;202m",
		Muted:       "\x1b[38;2;109;106;127m",
		Primary:     "\x1b[38;2;237;236;238m",
		Selected:    "\x1b[38;2;255;255;255m",
		Description: "\x1b[38;2;150;146;168m",
		SelectedBg:  "\x1b[48;2;61;55;94m",
		Ghost:       "\x1b[38;2;75;74;76m",
	}
	if p != want {
		t.Errorf("DefaultPalette() = %+v, want %+v", p, want)
	}
	for name, sgr := range map[string]string{
		"Border": p.Border, "Accent": p.Accent, "Muted": p.Muted,
		"Primary": p.Primary, "Selected": p.Selected, "Description": p.Description,
		"SelectedBg": p.SelectedBg, "Ghost": p.Ghost,
	} {
		if !strings.HasPrefix(sgr, "\x1b[") || !strings.HasSuffix(sgr, "m") {
			t.Errorf("Palette.%s = %q, not an SGR sequence", name, sgr)
		}
	}
}
