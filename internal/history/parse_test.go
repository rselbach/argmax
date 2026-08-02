package history

import (
	"testing"
	"time"
)

func TestParseBash(t *testing.T) {
	data := []byte("#1691234567\ngit status\nls -la\n#not-a-timestamp\n#1691239999\ngrep foo bar.txt\n\n#1691240000\n")
	got := ParseBash(data)

	want := []Entry{
		{Command: "git status", Time: time.Unix(1691234567, 0)},
		{Command: "ls -la"}, // no timestamp: marker consumed by previous command
		{Command: "grep foo bar.txt", Time: time.Unix(1691239999, 0)},
	}
	assertEntries(t, got, want)
}

func TestParseBashNoTimestamps(t *testing.T) {
	got := ParseBash([]byte("echo hello\ncd /tmp\n"))
	assertEntries(t, got, []Entry{{Command: "echo hello"}, {Command: "cd /tmp"}})
}

func TestParseBashCRLF(t *testing.T) {
	got := ParseBash([]byte("#1691234567\r\ngit status\r\n"))
	assertEntries(t, got, []Entry{{Command: "git status", Time: time.Unix(1691234567, 0)}})
}

func TestParseBashGarbage(t *testing.T) {
	got := ParseBash([]byte("\x00\x01garbage\xff\n###\n#12x34\nok command\n"))
	assertEntries(t, got, []Entry{
		{Command: "\x00\x01garbage\xff"},
		{Command: "ok command"}, // "###" and "#12x34" are ignored comments
	})
}

func TestParseZsh(t *testing.T) {
	data := []byte(": 1691234567:0;git status\n: 1691234700:12;docker compose up -d\nplain command\n")
	got := ParseZsh(data)
	want := []Entry{
		{Command: "git status", Time: time.Unix(1691234567, 0)},
		{Command: "docker compose up -d", Time: time.Unix(1691234700, 0)},
		{Command: "plain command"},
	}
	assertEntries(t, got, want)
}

func TestParseZshMalformed(t *testing.T) {
	data := []byte(": notanumber:x;looks extended but is not\n: 1691234567:5;\n: 1691234567\ncolon free\n\n")
	got := ParseZsh(data)
	want := []Entry{
		{Command: ": notanumber:x;looks extended but is not"}, // degraded to plain line
		// ": 1691234567:5;" has an empty command -> skipped
		{Command: ": 1691234567"}, // no ';' -> plain line
		{Command: "colon free"},
	}
	assertEntries(t, got, want)
}

func TestParseZshSemicolonInCommand(t *testing.T) {
	got := ParseZsh([]byte(": 1691234567:0;echo a; echo b\n"))
	assertEntries(t, got, []Entry{{Command: "echo a; echo b", Time: time.Unix(1691234567, 0)}})
}

func TestParseFish(t *testing.T) {
	data := []byte(`- cmd: git status
  when: 1691234567
- cmd: docker compose up -d
  when: 1691234700
  paths:
    - /tmp/project
- cmd: no timestamp here
`)
	got := ParseFish(data)
	want := []Entry{
		{Command: "git status", Time: time.Unix(1691234567, 0)},
		{Command: "docker compose up -d", Time: time.Unix(1691234700, 0)},
		{Command: "no timestamp here"},
	}
	assertEntries(t, got, want)
}

func TestParseFishEscapes(t *testing.T) {
	data := []byte("- cmd: echo one\\ntwo\n  when: 1691234567\n- cmd: echo C:\\\\tmp\\\\log\n- cmd: keep \\t literal\n")
	got := ParseFish(data)
	want := []Entry{
		{Command: "echo one\ntwo", Time: time.Unix(1691234567, 0)},
		{Command: `echo C:\tmp\log`},
		{Command: `keep \t literal`}, // unknown escapes keep their backslash
	}
	assertEntries(t, got, want)
}

func TestParseFishMalformed(t *testing.T) {
	data := []byte("garbage line\nwhen: 123\n- cmd:\n- cmd: real command\n  when: notanumber\n- cmd: \n")
	got := ParseFish(data)
	// "when:" before any record ignored; empty commands skipped; bad when -> zero time.
	assertEntries(t, got, []Entry{{Command: "real command"}})
}

func TestParseEmpty(t *testing.T) {
	for name, parse := range map[string]func([]byte) []Entry{
		"bash": ParseBash, "zsh": ParseZsh, "fish": ParseFish,
	} {
		if got := parse(nil); len(got) != 0 {
			t.Errorf("%s: Parse(nil) = %v, want empty", name, got)
		}
	}
}

func assertEntries(t *testing.T, got, want []Entry) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("got %d entries %v, want %d %v", len(got), got, len(want), want)
	}
	for i := range want {
		if got[i].Command != want[i].Command {
			t.Errorf("entry %d: Command = %q, want %q", i, got[i].Command, want[i].Command)
		}
		if !got[i].Time.Equal(want[i].Time) {
			t.Errorf("entry %d (%q): Time = %v, want %v", i, want[i].Command, got[i].Time, want[i].Time)
		}
	}
}
