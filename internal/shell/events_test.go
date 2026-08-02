package shell

import (
	"reflect"
	"testing"
)

func TestParseEventsAllTypes(t *testing.T) {
	data := []byte("cwd\t/tmp\x00prompt\t0\x00preexec\tgit status\x00postexec\t1\x00buffer\t5\tgit st")
	events, rest := ParseEvents(data)
	want := []Event{
		{Type: "cwd", Payload: "/tmp", Cursor: -1},
		{Type: "prompt", Payload: "0", Cursor: -1},
		{Type: "preexec", Payload: "git status", Cursor: -1},
		{Type: "postexec", Payload: "1", Cursor: -1},
	}
	if !reflect.DeepEqual(events, want) {
		t.Fatalf("events = %+v, want %+v", events, want)
	}
	if string(rest) != "buffer\t5\tgit st" {
		t.Fatalf("remainder = %q, want the partial buffer record", rest)
	}
}

func TestParseEventsBufferRecord(t *testing.T) {
	events, rest := ParseEvents([]byte("buffer\t12\tgit commit -m \"a\tb\"\x00"))
	if len(rest) != 0 {
		t.Fatalf("remainder = %q, want empty", rest)
	}
	if len(events) != 1 {
		t.Fatalf("got %d events, want 1", len(events))
	}
	ev := events[0]
	if ev.Type != "buffer" || ev.Cursor != 12 || ev.Text != "git commit -m \"a\tb\"" {
		t.Fatalf("buffer event = %+v", ev)
	}
	if ev.Payload != "12\tgit commit -m \"a\tb\"" {
		t.Fatalf("buffer Payload should be the raw payload, got %q", ev.Payload)
	}
}

func TestParseEventsPayloadMayContainTabs(t *testing.T) {
	// Split on the FIRST tab only: the rest is the payload verbatim.
	events, _ := ParseEvents([]byte("preexec\techo 'a\tb'\x00"))
	if len(events) != 1 || events[0].Payload != "echo 'a\tb'" {
		t.Fatalf("events = %+v", events)
	}
}

func TestParseEventsSkipsMalformed(t *testing.T) {
	data := []byte("\x00garbage\x00cwd\x00unknown\tx\x00buffer\tNaN\tx\x00buffer\t1\x00prompt\t0\x00")
	events, rest := ParseEvents(data)
	if len(rest) != 0 {
		t.Fatalf("remainder = %q, want empty", rest)
	}
	want := []Event{{Type: "prompt", Payload: "0", Cursor: -1}}
	if !reflect.DeepEqual(events, want) {
		t.Fatalf("events = %+v, want only %+v", events, want)
	}
}

func TestParseEventsEmptyAndExact(t *testing.T) {
	events, rest := ParseEvents(nil)
	if events != nil || len(rest) != 0 {
		t.Fatalf("empty input: events = %+v, rest = %q", events, rest)
	}

	events, rest = ParseEvents([]byte("prompt\t0\x00"))
	if len(rest) != 0 {
		t.Fatalf("fully consumed input should leave no remainder, got %q", rest)
	}
	if len(events) != 1 || events[0].Type != "prompt" {
		t.Fatalf("events = %+v", events)
	}
}

func TestParseEventsRemainderPrependedNextChunk(t *testing.T) {
	// Simulate a stream split mid-record: the caller prepends the remainder
	// to the next chunk.
	events, rest := ParseEvents([]byte("cwd\t/us"))
	if len(events) != 0 || string(rest) != "cwd\t/us" {
		t.Fatalf("first chunk: events = %+v, rest = %q", events, rest)
	}
	events, rest = ParseEvents(append(append([]byte{}, rest...), []byte("r/local\x00prompt\t2\x00")...))
	if len(rest) != 0 {
		t.Fatalf("remainder = %q, want empty", rest)
	}
	want := []Event{
		{Type: "cwd", Payload: "/usr/local", Cursor: -1},
		{Type: "prompt", Payload: "2", Cursor: -1},
	}
	if !reflect.DeepEqual(events, want) {
		t.Fatalf("events = %+v, want %+v", events, want)
	}
}
