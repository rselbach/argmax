package overlay

import (
	"bytes"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func TestGhostSuffix(t *testing.T) {
	cases := []struct {
		name   string
		buffer string
		text   string
		want   string
		ok     bool
	}{
		{"prefix", "gi", "git status", "t status", true},
		{"case-insensitive", "GI", "git status", "t status", true},
		{"uppercase text", "gi", "GIT status", "T status", true},
		{"full match no suffix", "git status", "git status", "", false},
		{"empty buffer", "", "git", "", false},
		{"not a prefix", "xyz", "git", "", false},
		{"buffer longer than text", "git ", "git", "", false},
		{"exact short match", "gi", "gi", "", false},
		{"empty text", "gi", "", "", false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, ok := GhostSuffix(c.buffer, core.Suggestion{Text: c.text})
			if got != c.want || ok != c.ok {
				t.Errorf("GhostSuffix(%q, %q) = (%q, %v), want (%q, %v)",
					c.buffer, c.text, got, ok, c.want, c.ok)
			}
		})
	}
}

func TestRenderGhost(t *testing.T) {
	pal := DefaultPalette()

	var buf bytes.Buffer
	RenderGhost(&buf, "t status", 20)
	want := pal.Ghost + "t status" + ansiReset + "\x1b[8D"
	if buf.String() != want {
		t.Errorf("RenderGhost = %q, want %q", buf.String(), want)
	}

	// Truncated to the available width with an ellipsis (UI-013); the
	// cursor-back matches the emitted cell width.
	buf.Reset()
	RenderGhost(&buf, "t status", 5)
	out := buf.String()
	if !strings.Contains(out, "…") {
		t.Error("truncated ghost missing ellipsis")
	}
	if !strings.HasSuffix(out, "\x1b[5D") {
		t.Errorf("truncated ghost should end with cursor-back 5: %q", out)
	}
	if !strings.HasPrefix(out, pal.Ghost) {
		t.Error("ghost missing ghost color")
	}

	buf.Reset()
	RenderGhost(&buf, "", 10)
	if buf.String() != "" {
		t.Errorf("empty suffix produced output: %q", buf.String())
	}

	buf.Reset()
	RenderGhost(&buf, "abc", 0)
	if buf.String() != "" {
		t.Errorf("zero width produced output: %q", buf.String())
	}
}

func TestClearGhost(t *testing.T) {
	var buf bytes.Buffer
	ClearGhost(&buf)
	if buf.String() != "\x1b[0K" {
		t.Errorf("ClearGhost = %q, want ESC[0K", buf.String())
	}
}
