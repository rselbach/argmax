package history

import (
	"strconv"
	"strings"
	"time"
)

// ParseBash parses raw Bash history file contents (HIST-003), oldest first.
// A line of the form `#<unix-ts>` is a timestamp marker applying to the NEXT
// command line; other `#` lines are ignored as comments. Commands without a
// preceding marker get a zero Time. Blank lines and garbage are skipped.
func ParseBash(data []byte) []Entry {
	var out []Entry
	var ts time.Time
	for _, line := range splitLines(data) {
		if line == "" {
			continue
		}
		if strings.HasPrefix(line, "#") {
			if n, err := strconv.ParseInt(line[1:], 10, 64); err == nil {
				ts = time.Unix(n, 0)
			}
			continue
		}
		out = append(out, Entry{Command: line, Time: ts})
		ts = time.Time{} // a marker applies to one command only
	}
	return out
}

// ParseZsh parses raw Zsh history file contents (HIST-003), oldest first.
// Extended-history lines look like `: 1691234567:0;git status` — Time comes
// from the unix timestamp, Command is the text after the first `;`. Plain
// lines without metadata are accepted with a zero Time. Malformed metadata
// degrades to a plain line; blank lines are skipped.
func ParseZsh(data []byte) []Entry {
	var out []Entry
	for _, line := range splitLines(data) {
		if line == "" {
			continue
		}
		if cmd, ts, ok := parseZshExtended(line); ok {
			if cmd == "" {
				continue
			}
			out = append(out, Entry{Command: cmd, Time: ts})
			continue
		}
		out = append(out, Entry{Command: line})
	}
	return out
}

// parseZshExtended splits an extended-history line into command and time.
// It reports ok=false when the line is not a well-formed extended entry
// (`: <ts>:<elapsed>;<cmd>` with numeric ts and elapsed).
func parseZshExtended(line string) (cmd string, ts time.Time, ok bool) {
	if !strings.HasPrefix(line, ": ") {
		return "", time.Time{}, false
	}
	rest := line[2:]
	i := strings.IndexByte(rest, ';')
	if i < 0 {
		return "", time.Time{}, false
	}
	meta, cmd := rest[:i], rest[i+1:]
	parts := strings.SplitN(meta, ":", 2)
	if len(parts) != 2 {
		return "", time.Time{}, false
	}
	sec, err := strconv.ParseInt(parts[0], 10, 64)
	if err != nil {
		return "", time.Time{}, false
	}
	if _, err := strconv.ParseInt(parts[1], 10, 64); err != nil {
		return "", time.Time{}, false
	}
	return cmd, time.Unix(sec, 0), true
}

// ParseFish parses raw Fish history file contents (HIST-003), oldest first.
// Records look like:
//
//   - cmd: git status
//     when: 1691234567
//
// `- cmd: ` lines start a record; an indented `when: <unix-ts>` line sets
// its Time. Other record fields (e.g. `paths`) are ignored. Inside the
// command, `\n` (backslash-n) unescapes to a real newline — so multi-line
// commands join into one Command — and `\\` unescapes to `\`; other
// backslashes are kept verbatim. Malformed lines and empty commands are
// skipped.
func ParseFish(data []byte) []Entry {
	var out []Entry
	var cur *Entry
	flush := func() {
		if cur != nil && cur.Command != "" {
			out = append(out, *cur)
		}
		cur = nil
	}
	for _, line := range splitLines(data) {
		if strings.HasPrefix(line, "- cmd: ") {
			flush()
			cur = &Entry{Command: unescapeFish(line[len("- cmd: "):])}
			continue
		}
		if cur == nil {
			continue
		}
		if t := strings.TrimSpace(line); strings.HasPrefix(t, "when: ") {
			if n, err := strconv.ParseInt(t[len("when: "):], 10, 64); err == nil {
				cur.Time = time.Unix(n, 0)
			}
		}
	}
	flush()
	return out
}

// unescapeFish resolves the two Fish history escapes: `\n` (backslash-n)
// into a newline and `\\` into a single backslash.
func unescapeFish(s string) string {
	if !strings.Contains(s, `\`) {
		return s
	}
	var b strings.Builder
	b.Grow(len(s))
	for i := 0; i < len(s); i++ {
		if s[i] == '\\' && i+1 < len(s) {
			switch s[i+1] {
			case 'n':
				b.WriteByte('\n')
				i++
				continue
			case '\\':
				b.WriteByte('\\')
				i++
				continue
			}
		}
		b.WriteByte(s[i])
	}
	return b.String()
}

// splitLines splits data on '\n' and strips one trailing '\r' per line so
// CRLF files parse cleanly.
func splitLines(data []byte) []string {
	lines := strings.Split(string(data), "\n")
	for i, line := range lines {
		lines[i] = strings.TrimSuffix(line, "\r")
	}
	return lines
}
