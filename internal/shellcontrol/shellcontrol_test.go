package shellcontrol

import (
	"bytes"
	"errors"
	"fmt"
	"strings"
	"testing"
	"unicode/utf8"
)

func mustControlRequestID(t *testing.T, value uint64) ControlRequestID {
	t.Helper()
	id, err := NewControlRequestID(value)
	if err != nil {
		t.Fatalf("NewControlRequestID(%d): %v", value, err)
	}
	return id
}

func mustProbeResyncRequestID(t *testing.T, value uint64) ProbeResyncRequestID {
	t.Helper()
	id, err := NewProbeResyncRequestID(value)
	if err != nil {
		t.Fatalf("NewProbeResyncRequestID(%d): %v", value, err)
	}
	return id
}

func mustReplacement(
	t *testing.T,
	request uint64,
	buffer string,
	cursor int,
) ReplacementControl {
	t.Helper()
	control, err := NewReplacementControl(
		mustControlRequestID(t, request),
		buffer,
		cursor,
	)
	if err != nil {
		t.Fatalf("NewReplacementControl(%d, %q, %d): %v", request, buffer, cursor, err)
	}
	return control
}

func decode(wire []byte) []DecodedControlFrame {
	decoder := NewDecoder()
	var frames []DecodedControlFrame
	decoder.Push(wire, func(frame DecodedControlFrame) {
		frames = append(frames, frame)
	})
	return frames
}

func assertReplacementFrame(t *testing.T, frame DecodedControlFrame, want ReplacementControl) {
	t.Helper()
	if frame.Kind() != DecodedReplacement {
		t.Fatalf("Kind() = %d, want DecodedReplacement; frame = %s", frame.Kind(), frame)
	}
	got, ok := frame.Replacement()
	if !ok {
		t.Fatal("Replacement() ok = false, want true")
	}
	if got != want {
		t.Errorf("Replacement() = %s, want %s", got, want)
	}
	if frame.Err() != nil {
		t.Errorf("Err() = %v, want nil", frame.Err())
	}
}

func assertProbeResyncFrame(t *testing.T, frame DecodedControlFrame, want ProbeResyncControl) {
	t.Helper()
	if frame.Kind() != DecodedProbeResync {
		t.Fatalf("Kind() = %d, want DecodedProbeResync; frame = %s", frame.Kind(), frame)
	}
	got, ok := frame.ProbeResync()
	if !ok {
		t.Fatal("ProbeResync() ok = false, want true")
	}
	if got != want {
		t.Errorf("ProbeResync() = %s, want %s", got, want)
	}
	if frame.Err() != nil {
		t.Errorf("Err() = %v, want nil", frame.Err())
	}
}

func frameError(t *testing.T, frame DecodedControlFrame) *FrameError {
	t.Helper()
	if frame.Kind() != DecodedRejected {
		t.Fatalf("Kind() = %d, want DecodedRejected; frame = %s", frame.Kind(), frame)
	}
	var got *FrameError
	if !errors.As(frame.Err(), &got) {
		t.Fatalf("Err() type = %T, want *FrameError", frame.Err())
	}
	return got
}

func assertFrameErrorKind(t *testing.T, frame DecodedControlFrame, want FrameErrorKind) {
	t.Helper()
	if got := frameError(t, frame); got.Kind() != want {
		t.Errorf("error kind = %d, want %d (%v)", got.Kind(), want, got)
	}
}

func TestReplacementRoundTripsUnicodeMultilineAndByteCursor(t *testing.T) {
	control := mustReplacement(t, 7, "echo 世界\nprintf 'Dean Pelton'", 11)
	encoded := control.Encode()
	frames := decode(encoded.Bytes())

	if len(frames) != 1 {
		t.Fatalf("decoded %d frames, want 1", len(frames))
	}
	assertReplacementFrame(t, frames[0], control)
	wire := encoded.Bytes()
	if !bytes.HasPrefix(wire, []byte(replacementControlPrefix)) {
		t.Errorf("wire does not start with %q", replacementControlPrefix)
	}
	if wire[len(wire)-1] != 0 {
		t.Error("wire does not end in NUL")
	}
	for _, char := range wire[:len(wire)-1] {
		if char > 0x7f {
			t.Fatalf("wire contains non-ASCII byte %#x", char)
		}
	}
	want := "argmax-control-v1:replace:7:11:32:" +
		"6563686f20e4b896e7958c0a7072696e746620274465616e2050656c746f6e27\x00"
	if string(wire) != want {
		t.Errorf("wire = %q, want %q", wire, want)
	}
}

func TestReplacementAccessorsAndBoundaryCursors(t *testing.T) {
	tests := map[string]struct {
		buffer string
		cursor int
		empty  bool
	}{
		"empty":       {buffer: "", cursor: 0, empty: true},
		"start":       {buffer: "Troy 🚀", cursor: 0},
		"before rune": {buffer: "Troy 🚀", cursor: 5},
		"end":         {buffer: "Troy 🚀", cursor: len("Troy 🚀")},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			control := mustReplacement(t, 7, tc.buffer, tc.cursor)
			if control.RequestID().Value() != 7 {
				t.Errorf("RequestID().Value() = %d, want 7", control.RequestID().Value())
			}
			if control.Buffer() != tc.buffer {
				t.Errorf("Buffer() = %q, want %q", control.Buffer(), tc.buffer)
			}
			if control.Cursor() != tc.cursor {
				t.Errorf("Cursor() = %d, want %d", control.Cursor(), tc.cursor)
			}
			if control.Len() != len(tc.buffer) {
				t.Errorf("Len() = %d, want %d", control.Len(), len(tc.buffer))
			}
			if control.Empty() != tc.empty {
				t.Errorf("Empty() = %t, want %t", control.Empty(), tc.empty)
			}

			frames := decode(control.Encode().Bytes())
			if len(frames) != 1 {
				t.Fatalf("decoded %d frames, want 1", len(frames))
			}
			assertReplacementFrame(t, frames[0], control)
		})
	}
}

func TestReplacementEncoderRejectsInvalidInput(t *testing.T) {
	_, err := NewReplacementControl(ControlRequestID{}, "", 0)
	var requestErr *ControlRequestIDError
	if !errors.As(err, &requestErr) || requestErr.Kind() != RequestIDZero {
		t.Fatalf("zero request ID error = %v, want RequestIDZero", err)
	}

	tests := map[string]struct {
		buffer      string
		cursor      int
		kind        EncodeErrorKind
		bytes       int
		limit       int
		bufferBytes int
	}{
		"NUL": {
			buffer: "a\x00b", cursor: 1, kind: EncodeNULBuffer,
		},
		"cursor splits UTF-8": {
			buffer: "é", cursor: 1, kind: EncodeInvalidCursor, bufferBytes: 2,
		},
		"cursor past end": {
			buffer: "abc", cursor: 4, kind: EncodeInvalidCursor, bufferBytes: 3,
		},
		"negative cursor": {
			buffer: "abc", cursor: -1, kind: EncodeInvalidCursor, bufferBytes: 3,
		},
		"oversized": {
			buffer: strings.Repeat("x", MaxControlBufferBytes+1),
			kind:   EncodeBufferTooLarge,
			bytes:  MaxControlBufferBytes + 1,
			limit:  MaxControlBufferBytes,
		},
		"invalid UTF-8": {
			buffer: string([]byte{0xff}), kind: EncodeInvalidUTF8, bufferBytes: 1,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := NewReplacementControl(mustControlRequestID(t, 1), tc.buffer, tc.cursor)
			var encodeErr *EncodeError
			if !errors.As(err, &encodeErr) {
				t.Fatalf("error type = %T, want *EncodeError", err)
			}
			if encodeErr.Kind() != tc.kind {
				t.Errorf("Kind() = %d, want %d", encodeErr.Kind(), tc.kind)
			}
			if encodeErr.Bytes() != tc.bytes {
				t.Errorf("Bytes() = %d, want %d", encodeErr.Bytes(), tc.bytes)
			}
			if encodeErr.Limit() != tc.limit {
				t.Errorf("Limit() = %d, want %d", encodeErr.Limit(), tc.limit)
			}
			if encodeErr.BufferBytes() != tc.bufferBytes {
				t.Errorf("BufferBytes() = %d, want %d", encodeErr.BufferBytes(), tc.bufferBytes)
			}
		})
	}
}

func TestRequestIdentifiersHaveIndependentStrictRanges(t *testing.T) {
	t.Run("replacement", func(t *testing.T) {
		tests := map[string]struct {
			value uint64
			kind  RequestIDErrorKind
		}{
			"zero":       {value: 0, kind: RequestIDZero},
			"over range": {value: MaxControlRequestID + 1, kind: RequestIDOutOfRange},
		}
		for name, tc := range tests {
			t.Run(name, func(t *testing.T) {
				_, err := NewControlRequestID(tc.value)
				var idErr *ControlRequestIDError
				if !errors.As(err, &idErr) {
					t.Fatalf("error type = %T, want *ControlRequestIDError", err)
				}
				if idErr.Kind() != tc.kind {
					t.Errorf("Kind() = %d, want %d", idErr.Kind(), tc.kind)
				}
				if idErr.Maximum() != MaxControlRequestID {
					t.Errorf("Maximum() = %d, want %d", idErr.Maximum(), MaxControlRequestID)
				}
			})
		}
		id := mustControlRequestID(t, MaxControlRequestID)
		if id.Value() != MaxControlRequestID {
			t.Errorf("Value() = %d, want %d", id.Value(), MaxControlRequestID)
		}
	})

	t.Run("probe resync", func(t *testing.T) {
		tests := map[string]struct {
			value uint64
			kind  RequestIDErrorKind
		}{
			"zero":       {value: 0, kind: RequestIDZero},
			"over range": {value: MaxProbeResyncRequestID + 1, kind: RequestIDOutOfRange},
		}
		for name, tc := range tests {
			t.Run(name, func(t *testing.T) {
				_, err := NewProbeResyncRequestID(tc.value)
				var idErr *ProbeResyncRequestIDError
				if !errors.As(err, &idErr) {
					t.Fatalf("error type = %T, want *ProbeResyncRequestIDError", err)
				}
				if idErr.Kind() != tc.kind {
					t.Errorf("Kind() = %d, want %d", idErr.Kind(), tc.kind)
				}
				if idErr.Maximum() != MaxProbeResyncRequestID {
					t.Errorf("Maximum() = %d, want %d", idErr.Maximum(), MaxProbeResyncRequestID)
				}
			})
		}
		id := mustProbeResyncRequestID(t, MaxProbeResyncRequestID)
		if id.Value() != MaxProbeResyncRequestID {
			t.Errorf("Value() = %d, want %d", id.Value(), MaxProbeResyncRequestID)
		}
	})
}

func TestProbeResyncRoundTripsEveryTwoPartPartition(t *testing.T) {
	control := NewProbeResyncControl(mustProbeResyncRequestID(t, 42))
	wire := control.Encode().Bytes()
	if string(wire) != "argmax-control-v1:resync:42\x00" {
		t.Fatalf("wire = %q, want exact resync grammar", wire)
	}
	if control.RequestID().Value() != 42 {
		t.Errorf("RequestID().Value() = %d, want 42", control.RequestID().Value())
	}

	for split := 0; split <= len(wire); split++ {
		decoder := NewDecoder()
		var frames []DecodedControlFrame
		emit := func(frame DecodedControlFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 1 {
			t.Fatalf("split %d decoded %d frames, want 1", split, len(frames))
		}
		assertProbeResyncFrame(t, frames[0], control)
		if decoder.PendingLen() != 0 {
			t.Errorf("split %d PendingLen() = %d, want 0", split, decoder.PendingLen())
		}
	}
}

func TestReplacementDecodesIdenticallyAcrossPartitions(t *testing.T) {
	control := mustReplacement(t, 7, "git commit\n--amend 🚀", 10)
	wire := control.Encode().Bytes()

	for split := 0; split <= len(wire); split++ {
		decoder := NewDecoder()
		var frames []DecodedControlFrame
		emit := func(frame DecodedControlFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 1 {
			t.Fatalf("split %d decoded %d frames, want 1", split, len(frames))
		}
		assertReplacementFrame(t, frames[0], control)
		if decoder.PendingLen() != 0 {
			t.Errorf("split %d PendingLen() = %d, want 0", split, decoder.PendingLen())
		}
	}

	for chunkSize := 1; chunkSize <= len(wire)+1; chunkSize++ {
		decoder := NewDecoder()
		var frames []DecodedControlFrame
		for offset := 0; offset < len(wire); offset += chunkSize {
			end := min(offset+chunkSize, len(wire))
			decoder.Push(wire[offset:end], func(frame DecodedControlFrame) {
				frames = append(frames, frame)
			})
		}
		if len(frames) != 1 {
			t.Fatalf("chunk size %d decoded %d frames, want 1", chunkSize, len(frames))
		}
		assertReplacementFrame(t, frames[0], control)
	}
}

func TestCoalescedFramesStayInWireOrder(t *testing.T) {
	first := mustReplacement(t, 1, "git", 3)
	resync := NewProbeResyncControl(mustProbeResyncRequestID(t, 9))
	second := mustReplacement(t, 2, "git status", 4)
	wire := append(first.Encode().Bytes(), resync.Encode().Bytes()...)
	wire = append(wire, second.Encode().Bytes()...)

	frames := decode(wire)
	if len(frames) != 3 {
		t.Fatalf("decoded %d frames, want 3", len(frames))
	}
	assertReplacementFrame(t, frames[0], first)
	assertProbeResyncFrame(t, frames[1], resync)
	assertReplacementFrame(t, frames[2], second)
}

func TestMalformedFramesAreIsolatedAndStrict(t *testing.T) {
	tests := map[string]struct {
		wire  string
		kind  FrameErrorKind
		bytes uint64
	}{
		"empty": {
			wire: "\x00", kind: FrameEmpty,
		},
		"wrong direction": {
			wire: "prompt-ready\x00", kind: FrameWrongDirection,
		},
		"unsupported version": {
			wire: "argmax-control-v2:replace:1:0:0:\x00", kind: FrameUnsupportedProtocol,
		},
		"unsupported operation": {
			wire: "argmax-control-v1:other:1:0:0:\x00", kind: FrameUnsupportedProtocol,
		},
		"resync extra field": {
			wire: "argmax-control-v1:resync:1:extra\x00", kind: FrameInvalidGrammar,
		},
		"resync missing request": {
			wire: "argmax-control-v1:resync:\x00", kind: FrameInvalidRequestID,
		},
		"resync leading zero": {
			wire: "argmax-control-v1:resync:01\x00", kind: FrameInvalidRequestID,
		},
		"resync zero": {
			wire: "argmax-control-v1:resync:0\x00", kind: FrameRequestIDOutOfRange,
		},
		"resync over shared range": {
			wire: "argmax-control-v1:resync:2147483648\x00", kind: FrameRequestIDOutOfRange,
		},
		"resync uint64 overflow": {
			wire: "argmax-control-v1:resync:18446744073709551616\x00", kind: FrameInvalidRequestID,
		},
		"replacement extra field": {
			wire: "argmax-control-v1:replace:1:0:0::extra\x00", kind: FrameInvalidGrammar,
		},
		"replacement missing field": {
			wire: "argmax-control-v1:replace:1:0:0\x00", kind: FrameInvalidGrammar,
		},
		"replacement leading-zero request": {
			wire: "argmax-control-v1:replace:01:0:0:\x00", kind: FrameInvalidRequestID,
		},
		"replacement zero request": {
			wire: "argmax-control-v1:replace:0:0:0:\x00", kind: FrameRequestIDOutOfRange,
		},
		"replacement over-range request": {
			wire: "argmax-control-v1:replace:2147483648:0:0:\x00", kind: FrameRequestIDOutOfRange,
		},
		"leading-zero cursor": {
			wire: "argmax-control-v1:replace:1:00:0:\x00", kind: FrameInvalidCursor,
		},
		"negative cursor": {
			wire: "argmax-control-v1:replace:1:-1:0:\x00", kind: FrameInvalidCursor,
		},
		"maximum uint64 cursor is out of range": {
			wire: "argmax-control-v1:replace:1:18446744073709551615:0:\x00", kind: FrameCursorOutOfRange,
		},
		"cursor overflow": {
			wire: "argmax-control-v1:replace:1:18446744073709551616:0:\x00", kind: FrameInvalidCursor,
		},
		"leading-zero length": {
			wire: "argmax-control-v1:replace:1:0:00:\x00", kind: FrameInvalidLength,
		},
		"maximum uint64 length is too large": {
			wire:  "argmax-control-v1:replace:1:0:18446744073709551615:\x00",
			kind:  FrameBufferTooLarge,
			bytes: ^uint64(0),
		},
		"length overflow": {
			wire: "argmax-control-v1:replace:1:0:18446744073709551616:\x00", kind: FrameInvalidLength,
		},
		"declared buffer too large": {
			wire:  "argmax-control-v1:replace:1:0:16385:\x00",
			kind:  FrameBufferTooLarge,
			bytes: MaxControlBufferBytes + 1,
		},
		"uppercase hex": {
			wire: "argmax-control-v1:replace:1:0:1:7A\x00", kind: FrameInvalidHex,
		},
		"short hex": {
			wire: "argmax-control-v1:replace:1:0:2:61\x00", kind: FrameHexLengthMismatch,
		},
		"long hex": {
			wire: "argmax-control-v1:replace:1:0:1:6162\x00", kind: FrameHexLengthMismatch,
		},
		"NUL buffer": {
			wire: "argmax-control-v1:replace:1:0:1:00\x00", kind: FrameNULBuffer,
		},
		"invalid UTF-8": {
			wire: "argmax-control-v1:replace:1:0:1:ff\x00", kind: FrameInvalidUTF8,
		},
		"cursor out of range": {
			wire: "argmax-control-v1:replace:1:3:2:c3a9\x00", kind: FrameCursorOutOfRange,
		},
		"cursor splits UTF-8": {
			wire: "argmax-control-v1:replace:1:1:2:c3a9\x00", kind: FrameCursorNotUTF8Boundary,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			frames := decode([]byte(tc.wire))
			if len(frames) != 1 {
				t.Fatalf("decoded %d frames, want 1", len(frames))
			}
			assertFrameErrorKind(t, frames[0], tc.kind)
			if tc.bytes != 0 {
				err := frameError(t, frames[0])
				if err.Bytes() != tc.bytes || err.Limit() != MaxControlBufferBytes {
					t.Errorf(
						"buffer bound = (%d, %d), want (%d, %d)",
						err.Bytes(), err.Limit(), tc.bytes, MaxControlBufferBytes,
					)
				}
			}
		})
	}
}

func TestMalformedFrameRecoveryPreservesWireOrderAcrossPartitions(t *testing.T) {
	valid := mustReplacement(t, 8, "Greendale", len("Greendale"))
	wire := append([]byte("bad\x00"), valid.Encode().Bytes()...)

	for split := 0; split <= len(wire); split++ {
		decoder := NewDecoder()
		var frames []DecodedControlFrame
		emit := func(frame DecodedControlFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 2 {
			t.Fatalf("split %d decoded %d frames, want 2", split, len(frames))
		}
		assertFrameErrorKind(t, frames[0], FrameWrongDirection)
		assertReplacementFrame(t, frames[1], valid)
	}
}

func TestOversizedFrameRetainsNoPayloadAndRecovers(t *testing.T) {
	decoder := NewDecoderWithFrameLimit(8)
	var frames []DecodedControlFrame
	emit := func(frame DecodedControlFrame) { frames = append(frames, frame) }
	decoder.Push([]byte("0123456789"), emit)
	if !decoder.Discarding() {
		t.Error("Discarding() = false, want true")
	}
	if decoder.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d, want 0", decoder.PendingLen())
	}
	decoder.Push([]byte("\x00bad\x00"), emit)

	if len(frames) != 2 {
		t.Fatalf("decoded %d frames, want 2", len(frames))
	}
	tooLarge := frameError(t, frames[0])
	if tooLarge.Kind() != FrameTooLarge || tooLarge.ObservedBytes() != 10 || tooLarge.Limit() != 8 {
		t.Errorf(
			"oversized error = %#v, want 10 observed bytes and limit 8",
			tooLarge,
		)
	}
	assertFrameErrorKind(t, frames[1], FrameWrongDirection)
	if decoder.Discarding() {
		t.Error("Discarding() = true after recovery, want false")
	}
}

func TestOversizedRecoveryIsPartitionInvariant(t *testing.T) {
	valid := NewProbeResyncControl(mustProbeResyncRequestID(t, 3))
	wire := append([]byte("0123456789\x00"), valid.Encode().Bytes()...)

	for split := 0; split <= len(wire); split++ {
		decoder := NewDecoderWithFrameLimit(8)
		var frames []DecodedControlFrame
		emit := func(frame DecodedControlFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 2 {
			t.Fatalf("split %d decoded %d frames, want 2", split, len(frames))
		}
		err := frameError(t, frames[0])
		if err.Kind() != FrameTooLarge || err.ObservedBytes() != 10 || err.Limit() != 8 {
			t.Errorf("split %d oversized error = %#v", split, err)
		}
		// The configured limit also applies to later frames, so this resync frame
		// is independently oversized rather than parsed.
		assertFrameErrorKind(t, frames[1], FrameTooLarge)
	}
}

func TestDecoderLimitIsCappedAndExact(t *testing.T) {
	tests := map[string]struct {
		limit int
		want  int
	}{
		"negative":      {limit: -1, want: 0},
		"zero":          {limit: 0, want: 0},
		"small":         {limit: 8, want: 8},
		"protocol max":  {limit: MaxControlFrameBytes, want: MaxControlFrameBytes},
		"above maximum": {limit: MaxControlFrameBytes + 1, want: MaxControlFrameBytes},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			decoder := NewDecoderWithFrameLimit(tc.limit)
			if decoder.FrameLimit() != tc.want {
				t.Errorf("FrameLimit() = %d, want %d", decoder.FrameLimit(), tc.want)
			}
		})
	}

	decoder := NewDecoderWithFrameLimit(3)
	var frames []DecodedControlFrame
	decoder.Push([]byte("bad\x00"), func(frame DecodedControlFrame) {
		frames = append(frames, frame)
	})
	if len(frames) != 1 {
		t.Fatalf("decoded %d frames, want 1", len(frames))
	}
	assertFrameErrorKind(t, frames[0], FrameWrongDirection)
}

func TestFinishRejectsPartialWithoutRetainingContent(t *testing.T) {
	decoder := NewDecoder()
	decoder.Push([]byte("secret-control"), func(DecodedControlFrame) {
		t.Fatal("unterminated input emitted a frame")
	})
	if decoder.PendingLen() != 14 {
		t.Fatalf("PendingLen() = %d, want 14", decoder.PendingLen())
	}
	frame, ok := decoder.Finish()
	if !ok {
		t.Fatal("Finish() ok = false, want true")
	}
	err := frameError(t, frame)
	if err.Kind() != FrameTruncated || err.ObservedBytes() != 14 {
		t.Errorf("Finish() error = %#v, want 14-byte TruncatedFrame", err)
	}
	if decoder.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d after Finish, want 0", decoder.PendingLen())
	}
	if _, ok := decoder.Finish(); ok {
		t.Error("second Finish() ok = true, want false")
	}
}

func TestFinishRejectsOversizedUnterminatedFrame(t *testing.T) {
	decoder := NewDecoderWithFrameLimit(2)
	decoder.Push([]byte("secret"), func(DecodedControlFrame) {
		t.Fatal("unterminated input emitted a frame")
	})
	frame, ok := decoder.Finish()
	if !ok {
		t.Fatal("Finish() ok = false, want true")
	}
	err := frameError(t, frame)
	if err.Kind() != FrameTooLarge || err.ObservedBytes() != 6 || err.Limit() != 2 {
		t.Errorf("Finish() error = %#v, want 6-byte FrameTooLarge with limit 2", err)
	}
	if decoder.Discarding() {
		t.Error("Discarding() = true after Finish, want false")
	}
	if _, ok := decoder.Finish(); ok {
		t.Error("second Finish() ok = true, want false")
	}
}

func TestFormattingNeverExposesReplacementContent(t *testing.T) {
	secret := "Dean Pelton private token"
	control := mustReplacement(t, 7, secret, len(secret))
	encoded := control.Encode()
	resync := NewProbeResyncControl(mustProbeResyncRequestID(t, 17))
	decoder := NewDecoder()
	decoder.Push(encoded.Bytes()[:8], func(DecodedControlFrame) {
		t.Fatal("partial frame emitted a result")
	})
	decoded := DecodedControlFrame{kind: DecodedReplacement, replacement: control}

	values := map[string]any{
		"replacement": control,
		"encoded":     encoded,
		"resync":      resync,
		"decoder":     decoder,
		"decoded":     decoded,
	}
	for name, value := range values {
		for _, format := range []string{"%v", "%+v", "%#v", "%s"} {
			formatted := fmt.Sprintf(format, value)
			if strings.Contains(formatted, secret) {
				t.Errorf("%s formatted with %q exposed replacement: %s", name, format, formatted)
			}
		}
	}
	if !strings.Contains(control.String(), fmt.Sprintf("buffer_bytes: %d", len(secret))) {
		t.Errorf("ReplacementControl.String() = %q, want byte count", control.String())
	}
	if strings.Contains(decoder.String(), string(encoded.Bytes()[:8])) {
		t.Errorf("Decoder.String() exposed pending bytes: %s", decoder)
	}
}

func TestErrorFormattingIsContentFree(t *testing.T) {
	tests := map[string]struct {
		err      error
		want     string
		goString string
	}{
		"replacement ID zero": {
			err:      &ControlRequestIDError{kind: RequestIDZero},
			want:     "shell control request identifier is zero",
			goString: "Zero",
		},
		"replacement ID range": {
			err:      &ControlRequestIDError{kind: RequestIDOutOfRange},
			want:     "shell control request identifier exceeds shared maximum 2147483647",
			goString: "OutOfRange { maximum: 2147483647 }",
		},
		"probe ID zero": {
			err:      &ProbeResyncRequestIDError{kind: RequestIDZero},
			want:     "probe resync request identifier is zero",
			goString: "Zero",
		},
		"probe ID range": {
			err:      &ProbeResyncRequestIDError{kind: RequestIDOutOfRange},
			want:     "probe resync request identifier exceeds shared maximum 2147483647",
			goString: "OutOfRange { maximum: 2147483647 }",
		},
		"encode too large": {
			err: &EncodeError{
				kind: EncodeBufferTooLarge, bytes: 16385, limit: 16384,
			},
			want:     "shell control buffer is 16385 bytes; limit is 16384",
			goString: "BufferTooLarge { bytes: 16385, limit: 16384 }",
		},
		"encode NUL": {
			err: &EncodeError{kind: EncodeNULBuffer}, want: "shell control buffer contains NUL",
			goString: "NulBuffer",
		},
		"encode cursor": {
			err:      &EncodeError{kind: EncodeInvalidCursor, cursor: 1, bufferBytes: 2},
			want:     "shell control cursor 1 is invalid for 2 bytes",
			goString: "InvalidCursor { cursor: 1, buffer_bytes: 2 }",
		},
		"frame too large": {
			err:      &FrameError{kind: FrameTooLarge, observedBytes: 10, limit: 8},
			want:     "shell control frame is 10 bytes; limit is 8",
			goString: "FrameTooLarge { observed_bytes: 10, limit: 8 }",
		},
		"frame truncated": {
			err:      &FrameError{kind: FrameTruncated, observedBytes: 14},
			want:     "shell control frame ended after 14 bytes without a terminator",
			goString: "TruncatedFrame { observed_bytes: 14 }",
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := tc.err.Error(); got != tc.want {
				t.Errorf("Error() = %q, want %q", got, tc.want)
			}
			if got := fmt.Sprintf("%#v", tc.err); got != tc.goString {
				t.Errorf("GoString() = %q, want %q", got, tc.goString)
			}
		})
	}
}

func TestMaximumFrameMatchesEncodedUpperBound(t *testing.T) {
	if MaxControlBufferBytes != 16_384 {
		t.Errorf("MaxControlBufferBytes = %d, want 16384", MaxControlBufferBytes)
	}
	if MaxControlFrameBytes != 32_817 {
		t.Errorf("MaxControlFrameBytes = %d, want 32817", MaxControlFrameBytes)
	}
	if MaxControlWireBytes != 32_818 {
		t.Errorf("MaxControlWireBytes = %d, want 32818", MaxControlWireBytes)
	}
	control := mustReplacement(
		t,
		MaxControlRequestID,
		strings.Repeat("x", MaxControlBufferBytes),
		MaxControlBufferBytes,
	)
	if control.Encode().Len()-1 != MaxControlFrameBytes {
		t.Errorf(
			"maximum replacement frame = %d bytes, want %d",
			control.Encode().Len()-1,
			MaxControlFrameBytes,
		)
	}
	resync := NewProbeResyncControl(mustProbeResyncRequestID(t, MaxProbeResyncRequestID))
	if resync.Encode().Len()-1 >= MaxControlFrameBytes {
		t.Errorf("maximum resync frame = %d, want below %d", resync.Encode().Len()-1, MaxControlFrameBytes)
	}

	frames := decode(control.Encode().Bytes())
	if len(frames) != 1 {
		t.Fatalf("maximum wire decoded %d frames, want 1", len(frames))
	}
	assertReplacementFrame(t, frames[0], control)
}

func TestEncodedBytesReturnsIndependentCopy(t *testing.T) {
	encoded := mustReplacement(t, 1, "Community", len("Community")).Encode()
	first := encoded.Bytes()
	first[0] = 'X'
	second := encoded.Bytes()
	if second[0] != 'a' {
		t.Errorf("mutating Bytes result changed frame: first byte = %q", second[0])
	}
	if encoded.Empty() {
		t.Error("Empty() = true, want false")
	}
	if encoded.Len() != len(second) {
		t.Errorf("Len() = %d, want %d", encoded.Len(), len(second))
	}
}

func TestUTF8CursorValidationUsesByteOffsets(t *testing.T) {
	buffer := "世界🚀"
	valid := map[int]bool{
		0:          true,
		len("世"):   true,
		len("世界"):  true,
		len("世界🚀"): true,
	}
	for cursor := 0; cursor <= len(buffer); cursor++ {
		_, err := NewReplacementControl(mustControlRequestID(t, 1), buffer, cursor)
		if (err == nil) != valid[cursor] {
			t.Errorf("cursor %d valid = %t, want %t", cursor, err == nil, valid[cursor])
		}
		if err == nil && !utf8.ValidString(buffer[:cursor]) {
			t.Errorf("cursor %d accepted but prefix is invalid UTF-8", cursor)
		}
	}
}
