package session

import (
	"bytes"
	"errors"
	"io"
	"reflect"
	"strings"
	"testing"
)

func TestReadEventRecordsRecoversAfterOversizedRecord(t *testing.T) {
	input := "cwd:/before\x00pre:" + strings.Repeat("x", maxEventRecordSize) +
		"\x00post:0\x00ready:\x00"
	var records []string
	dropped := 0
	err := readEventRecords(bytes.NewBufferString(input), func(record string) bool {
		records = append(records, record)
		return true
	}, func() {
		dropped++
	})
	if !errors.Is(err, io.EOF) {
		t.Fatalf("readEventRecords() error = %v, want EOF", err)
	}
	if dropped != 1 {
		t.Errorf("dropped %d oversized records, want 1", dropped)
	}
	want := []string{"cwd:/before", "post:0", "ready:"}
	if !reflect.DeepEqual(records, want) {
		t.Errorf("records after oversized event = %#v, want %#v", records, want)
	}
}

func TestReadEventRecordsAcceptsMaximumSize(t *testing.T) {
	want := strings.Repeat("x", maxEventRecordSize)
	var got string
	err := readEventRecords(bytes.NewBufferString(want+"\x00"), func(record string) bool {
		got = record
		return true
	}, func() {
		t.Fatal("maximum-sized event was discarded")
	})
	if !errors.Is(err, io.EOF) {
		t.Fatalf("readEventRecords() error = %v, want EOF", err)
	}
	if got != want {
		t.Errorf("maximum-sized record length = %d, want %d", len(got), len(want))
	}
}
