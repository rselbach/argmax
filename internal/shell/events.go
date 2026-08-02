package shell

import (
	"bytes"
	"strconv"
)

// HookFDEnv names the environment variable telling the integration hooks
// which inherited file descriptor carries the event pipe (SH-006). The
// session passes a pipe as an inherited fd (default "3") and sets this
// variable accordingly.
const HookFDEnv = "ARGMAX_HOOK_FD"

// Event is one decoded hook record. Records are NUL-delimited and have the
// form "<type>\t<payload>"; the buffer record has two payload fields,
// "<cursor>\t<text>".
type Event struct {
	Type    string // "cwd" | "prompt" | "preexec" | "postexec" | "buffer"
	Payload string // cwd: absolute path; prompt/postexec: exit status; preexec: command line; buffer: raw "<cursor>\t<text>" (also decoded into Cursor and Text)
	Cursor  int    // buffer only: cursor byte offset within Text (-1 when unset)
	Text    string // buffer only: full buffer text
}

// ParseEvents decodes NUL-delimited hook records from data, returning the
// events and the unconsumed remainder (a partial trailing record, which the
// caller should prepend to the next chunk). Malformed records are skipped.
// The payload is split from the type on the first tab only, so payloads may
// contain tabs; the buffer payload is split into its two fields on the first
// two tabs.
func ParseEvents(data []byte) ([]Event, []byte) {
	var events []Event
	for {
		i := bytes.IndexByte(data, 0)
		if i < 0 {
			return events, data
		}
		if ev, ok := parseRecord(data[:i]); ok {
			events = append(events, ev)
		}
		data = data[i+1:]
	}
}

// parseRecord decodes one record (without its NUL terminator).
func parseRecord(rec []byte) (Event, bool) {
	typ, rest, found := bytes.Cut(rec, []byte("\t"))
	if !found {
		return Event{}, false
	}
	switch t := string(typ); t {
	case "cwd", "prompt", "preexec", "postexec":
		return Event{Type: t, Payload: string(rest), Cursor: -1}, true
	case "buffer":
		cursor, text, found := bytes.Cut(rest, []byte("\t"))
		if !found {
			return Event{}, false
		}
		n, err := strconv.Atoi(string(cursor))
		if err != nil {
			return Event{}, false
		}
		return Event{Type: t, Payload: string(rest), Cursor: n, Text: string(text)}, true
	}
	return Event{}, false
}
