package shellevents

import (
	"bytes"
	"fmt"
	"math"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/shellcontrol"
)

func testDecoder() *Decoder { return NewDecoder(InitialStreamEpoch()) }

func decodeEvents(decoder *Decoder, wire []byte) []DecodedFrame {
	var frames []DecodedFrame
	decoder.Push(wire, func(frame DecodedFrame) { frames = append(frames, frame) })
	return frames
}

func decodedEvent(t *testing.T, frame DecodedFrame) ShellEvent {
	t.Helper()
	sequenced, ok := frame.Event()
	if !ok {
		t.Fatalf("frame = %s, want event", frame)
	}
	return sequenced.Event()
}

func decodedError(t *testing.T, frame DecodedFrame) *FrameError {
	t.Helper()
	rejection, ok := frame.Rejection()
	if !ok {
		t.Fatalf("frame = %s, want rejection", frame)
	}
	return rejection.Error()
}

func assertFrameKind(t *testing.T, frame DecodedFrame, want FrameErrorKind) {
	t.Helper()
	if got := decodedError(t, frame).Kind(); got != want {
		t.Errorf("error kind = %d, want %d (%v)", got, want, decodedError(t, frame))
	}
}

func TestDecoderDecodesChunkBoundariesAndMultipleEvents(t *testing.T) {
	wire := []byte("capability:sync-probe:40\x00prompt-ready\x00" +
		"probe-buffer:b:41:3:git status\x00command-start:git status\x00command-stop:17\x00")
	decoder := testDecoder()
	var frames []DecodedFrame
	for _, char := range wire {
		decoder.Push([]byte{char}, func(frame DecodedFrame) { frames = append(frames, frame) })
		if decoder.PendingLen() > decoder.FrameLimit() {
			t.Fatalf("PendingLen() = %d, above limit %d", decoder.PendingLen(), decoder.FrameLimit())
		}
	}
	if len(frames) != 5 {
		t.Fatalf("decoded %d frames, want 5", len(frames))
	}
	if frames[0].Position().Sequence().Low() != 0 || frames[4].Position().Sequence().Low() != 4 {
		t.Errorf("sequences = %d..%d, want 0..4", frames[0].Position().Sequence().Low(), frames[4].Position().Sequence().Low())
	}
	announcement, ok := decodedEvent(t, frames[0]).Capability()
	if !ok || announcement.Capability() != BufferSyncProbe {
		t.Fatalf("first event = %s, want probe capability", decodedEvent(t, frames[0]))
	}
	nonce, ok := announcement.LastProbeNonce()
	if !ok || nonce.Value() != 40 {
		t.Errorf("LastProbeNonce() = (%d, %t), want (40, true)", nonce.Value(), ok)
	}
	if decodedEvent(t, frames[1]).Kind() != EventPromptReady {
		t.Errorf("second event kind = %d, want EventPromptReady", decodedEvent(t, frames[1]).Kind())
	}
	snapshot, ok := decodedEvent(t, frames[2]).Buffer()
	if !ok {
		t.Fatal("third event is not a buffer")
	}
	probe, ok := snapshot.ProbeNonce()
	if !ok || probe.Value() != 41 {
		t.Errorf("ProbeNonce() = (%d, %t), want (41, true)", probe.Value(), ok)
	}
	if decodedEvent(t, frames[3]).Kind() != EventCommandStart {
		t.Errorf("fourth event kind = %d, want command start", decodedEvent(t, frames[3]).Kind())
	}
	status, ok := decodedEvent(t, frames[4]).Status()
	if !ok || status.Value() != 17 {
		t.Errorf("Status() = (%d, %t), want (17, true)", status.Value(), ok)
	}
}

func TestDecoderIsPartitionInvariant(t *testing.T) {
	wire := []byte("capability:sync-probe:40\x00prompt-ready\x00" +
		"probe-buffer:c:41:2:a☃b\x00command-start:a\xffb\x00command-stop:17\x00")
	want := decodeEvents(testDecoder(), wire)
	if len(want) != 5 {
		t.Fatalf("baseline decoded %d frames, want 5", len(want))
	}

	for split := 0; split <= len(wire); split++ {
		decoder := testDecoder()
		var got []DecodedFrame
		emit := func(frame DecodedFrame) { got = append(got, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if gotString := fmt.Sprintf("%#v", got); gotString != fmt.Sprintf("%#v", want) {
			t.Fatalf("split %d decoded %s, want %s", split, gotString, fmt.Sprintf("%#v", want))
		}
		if decoder.PendingLen() != 0 {
			t.Errorf("split %d PendingLen() = %d, want 0", split, decoder.PendingLen())
		}
	}

	for chunkSize := 1; chunkSize <= len(wire)+1; chunkSize++ {
		decoder := testDecoder()
		var got []DecodedFrame
		for offset := 0; offset < len(wire); offset += chunkSize {
			end := min(offset+chunkSize, len(wire))
			decoder.Push(wire[offset:end], func(frame DecodedFrame) { got = append(got, frame) })
		}
		if fmt.Sprintf("%#v", got) != fmt.Sprintf("%#v", want) {
			t.Fatalf("chunk size %d changed decoding", chunkSize)
		}
	}
}

func TestDecoderWorkingDirectoryAndReloadValidation(t *testing.T) {
	valid := decodeEvents(testDecoder(), []byte("cwd:/tmp/Greendale Community College\x00reload-request:42\x00"))
	directory, ok := decodedEvent(t, valid[0]).WorkingDirectory()
	if !ok || !bytes.Equal(directory.Bytes(), []byte("/tmp/Greendale Community College")) {
		t.Errorf("WorkingDirectory() = (%q, %t)", directory.Bytes(), ok)
	}
	request, ok := decodedEvent(t, valid[1]).ReloadRequest()
	if !ok || request.Nonce() != 42 {
		t.Errorf("ReloadRequest() = (%d, %t), want (42, true)", request.Nonce(), ok)
	}

	tests := map[string]struct {
		wire string
		kind FrameErrorKind
	}{
		"relative CWD":        {wire: "cwd:relative\x00", kind: FrameInvalidWorkingDirectory},
		"empty CWD":           {wire: "cwd:\x00", kind: FrameInvalidWorkingDirectory},
		"oversized CWD":       {wire: "cwd:/" + strings.Repeat("x", maxWorkingDirectoryBytes) + "\x00", kind: FrameInvalidWorkingDirectory},
		"missing reload":      {wire: "reload-request:\x00", kind: FrameInvalidReloadRequest},
		"leading zero reload": {wire: "reload-request:01\x00", kind: FrameInvalidReloadRequest},
		"zero reload":         {wire: "reload-request:0\x00", kind: FrameInvalidReloadRequest},
		"overflow reload":     {wire: "reload-request:4294967296\x00", kind: FrameInvalidReloadRequest},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			frames := decodeEvents(testDecoder(), []byte(tc.wire))
			assertFrameKind(t, frames[0], tc.kind)
		})
	}

	rawPath := []byte("cwd:/tmp/Greendale-\xff\x00")
	frames := decodeEvents(testDecoder(), rawPath)
	directory, ok = decodedEvent(t, frames[0]).WorkingDirectory()
	if !ok || !bytes.Equal(directory.Bytes(), []byte("/tmp/Greendale-\xff")) {
		t.Errorf("non-UTF8 path = %q, want exact bytes", directory.Bytes())
	}

	boundary := append([]byte("cwd:/"), bytes.Repeat([]byte{'x'}, maxWorkingDirectoryBytes-1)...)
	boundary = append(boundary, 0)
	frames = decodeEvents(testDecoder(), boundary)
	directory, ok = decodedEvent(t, frames[0]).WorkingDirectory()
	if !ok || len(directory.Bytes()) != maxWorkingDirectoryBytes {
		t.Errorf("maximum CWD = (%d bytes, %t), want (%d, true)", len(directory.Bytes()), ok, maxWorkingDirectoryBytes)
	}
}

func TestDecoderValidatesAndConvertsBufferCursors(t *testing.T) {
	tests := map[string]struct {
		wire       []byte
		kind       FrameErrorKind
		wantBytes  []byte
		wantCursor int
		wantProbe  uint64
		hasProbe   bool
	}{
		"byte boundary": {
			wire: []byte("buffer:b:2:éx\x00"), wantBytes: []byte("éx"), wantCursor: 2,
		},
		"leading-zero byte cursor": {
			wire: []byte("buffer:b:002:éx\x00"), wantBytes: []byte("éx"), wantCursor: 2,
		},
		"byte split": {
			wire: []byte("buffer:b:1:éx\x00"), kind: FrameCursorNotUTF8Boundary,
		},
		"byte out of range": {
			wire: []byte("buffer:b:4:éx\x00"), kind: FrameCursorOutOfRange,
		},
		"character cursor": {
			wire: []byte("buffer:c:2:a☃b\x00"), wantBytes: []byte("a☃b"), wantCursor: 4,
		},
		"character end": {
			wire: []byte("buffer:c:3:a☃b\x00"), wantBytes: []byte("a☃b"), wantCursor: 5,
		},
		"character out of range": {
			wire: []byte("buffer:c:4:a☃b\x00"), kind: FrameCursorOutOfRange,
		},
		"Fish trailing newline": {
			wire: []byte("probe-buffer:f:9:5:echo\n\n\x00"), wantBytes: []byte("echo\n"), wantCursor: 5, wantProbe: 9, hasProbe: true,
		},
		"Fish missing print terminator": {
			wire: []byte("probe-buffer:f:10:0:\x00"), kind: FrameMissingFishPrintTerminator,
		},
		"invalid UTF-8": {
			wire: []byte("buffer:b:0:\xff\x00"), kind: FrameInvalidBufferUTF8,
		},
		"missing cursor": {
			wire: []byte("buffer:b::x\x00"), kind: FrameMissingCursor,
		},
		"nondecimal cursor": {
			wire: []byte("buffer:b:x:x\x00"), kind: FrameNonDecimalCursor,
		},
		"cursor overflow": {
			wire: []byte("buffer:b:18446744073709551616:x\x00"), kind: FrameCursorOutOfRange,
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			frames := decodeEvents(testDecoder(), tc.wire)
			if tc.kind != 0 {
				assertFrameKind(t, frames[0], tc.kind)
				return
			}
			snapshot, ok := decodedEvent(t, frames[0]).Buffer()
			if !ok {
				t.Fatal("event is not a buffer")
			}
			if !bytes.Equal(snapshot.Bytes(), tc.wantBytes) || snapshot.Cursor() != tc.wantCursor {
				t.Errorf("snapshot = (%q, %d), want (%q, %d)", snapshot.Bytes(), snapshot.Cursor(), tc.wantBytes, tc.wantCursor)
			}
			probe, hasProbe := snapshot.ProbeNonce()
			if hasProbe != tc.hasProbe || hasProbe && probe.Value() != tc.wantProbe {
				t.Errorf("ProbeNonce() = (%d, %t), want (%d, %t)", probe.Value(), hasProbe, tc.wantProbe, tc.hasProbe)
			}
			if !bytes.Equal(snapshot.BeforeCursor(), tc.wantBytes[:tc.wantCursor]) ||
				!bytes.Equal(snapshot.AfterCursor(), tc.wantBytes[tc.wantCursor:]) {
				t.Error("cursor slices do not partition exact bytes")
			}
			text, valid := snapshot.StringValue()
			if !valid || text != string(tc.wantBytes) {
				t.Errorf("StringValue() = (%q, %t), want valid exact text", text, valid)
			}
		})
	}
}

func TestDecoderProbeNonceAndResyncValidation(t *testing.T) {
	valid := decodeEvents(testDecoder(), []byte("probe-resync:7:00042\x00"))
	response, ok := decodedEvent(t, valid[0]).ProbeResync()
	if !ok || response.RequestID().Value() != 7 || response.LastProbeNonce().Value() != 42 {
		t.Errorf("ProbeResync() = (%v, %t), want request 7 nonce 42", response, ok)
	}

	tests := map[string]struct {
		wire string
		kind FrameErrorKind
	}{
		"missing capability nonce":  {wire: "capability:sync-probe:\x00", kind: FrameMissingProbeNonce},
		"nondigit snapshot nonce":   {wire: "probe-buffer:b:x:0:x\x00", kind: FrameNonDecimalProbeNonce},
		"overflow capability nonce": {wire: "capability:sync-probe:18446744073709551616\x00", kind: FrameProbeNonceOutOfRange},
		"missing resync field":      {wire: "probe-resync:7\x00", kind: FrameInvalidProbeResyncGrammar},
		"empty resync request":      {wire: "probe-resync::1\x00", kind: FrameInvalidProbeResyncRequestID},
		"leading-zero request":      {wire: "probe-resync:01:1\x00", kind: FrameInvalidProbeResyncRequestID},
		"zero request":              {wire: "probe-resync:0:1\x00", kind: FrameProbeResyncRequestIDOutOfRange},
		"over-bound request":        {wire: "probe-resync:2147483648:1\x00", kind: FrameProbeResyncRequestIDOutOfRange},
		"request uint64 overflow":   {wire: "probe-resync:18446744073709551616:1\x00", kind: FrameInvalidProbeResyncRequestID},
		"extra resync field":        {wire: "probe-resync:1:2:3\x00", kind: FrameInvalidProbeResyncGrammar},
		"missing last nonce":        {wire: "probe-resync:1:\x00", kind: FrameMissingProbeNonce},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			frames := decodeEvents(testDecoder(), []byte(tc.wire))
			assertFrameKind(t, frames[0], tc.kind)
		})
	}
}

func TestDecoderSubmittedCommandPreservesArbitraryBytes(t *testing.T) {
	frames := decodeEvents(testDecoder(), []byte("command-start:a\xffb\x00"))
	command, ok := decodedEvent(t, frames[0]).Command()
	if !ok || !bytes.Equal(command.Bytes(), []byte("a\xffb")) {
		t.Fatalf("Command() = (%q, %t), want exact non-UTF8 bytes", command.Bytes(), ok)
	}
	if text, valid := command.StringValue(); valid || text != "" {
		t.Errorf("StringValue() = (%q, %t), want invalid", text, valid)
	}
	copied := command.Bytes()
	copied[0] = 'X'
	if bytes.Equal(command.Bytes(), copied) {
		t.Error("mutating Bytes() result changed SubmittedCommand")
	}
}

func TestDecoderExitStatusAndCommandValidation(t *testing.T) {
	tests := map[string]struct {
		wire       string
		kind       FrameErrorKind
		wantStatus uint8
		wantKind   ShellEventKind
	}{
		"success":       {wire: "command-stop:0\x00", wantKind: EventCommandStop},
		"maximum":       {wire: "command-stop:255\x00", wantKind: EventCommandStop, wantStatus: 255},
		"leading zeros": {wire: "command-stop:0001\x00", wantKind: EventCommandStop, wantStatus: 1},
		"over range":    {wire: "command-stop:256\x00", kind: FrameExitStatusOutOfRange},
		"negative":      {wire: "command-stop:-1\x00", kind: FrameNonDecimalExitStatus},
		"missing":       {wire: "command-stop:\x00", kind: FrameMissingExitStatus},
		"empty command": {wire: "command-start:\x00", kind: FrameEmptySubmittedCommand},
		"unknown start": {wire: "command-start-unknown\x00", wantKind: EventCommandStartUnknown},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			frames := decodeEvents(testDecoder(), []byte(tc.wire))
			if tc.kind != 0 {
				assertFrameKind(t, frames[0], tc.kind)
				return
			}
			event := decodedEvent(t, frames[0])
			if event.Kind() != tc.wantKind {
				t.Fatalf("event kind = %d, want %d", event.Kind(), tc.wantKind)
			}
			if event.Kind() == EventCommandStop {
				status, _ := event.Status()
				if status.Value() != tc.wantStatus {
					t.Errorf("status = %d, want %d", status.Value(), tc.wantStatus)
				}
				if status.Success() != (tc.wantStatus == 0) {
					t.Errorf("Success() = %t for status %d", status.Success(), status.Value())
				}
			}
		})
	}
}

func TestDecoderMalformedFramesAreIsolated(t *testing.T) {
	frames := decodeEvents(testDecoder(), []byte("\x00wat\x00buffer:b:0:ok\x00"))
	if len(frames) != 3 {
		t.Fatalf("decoded %d frames, want 3", len(frames))
	}
	assertFrameKind(t, frames[0], FrameEmpty)
	assertFrameKind(t, frames[1], FrameUnknownEvent)
	if decodedEvent(t, frames[2]).Kind() != EventBuffer {
		t.Errorf("third event = %s, want buffer", decodedEvent(t, frames[2]))
	}
	for index, frame := range frames {
		if frame.Position().Sequence().Low() != uint64(index) {
			t.Errorf("frame %d sequence = %d", index, frame.Position().Sequence().Low())
		}
	}
}

func TestDecoderMalformedRecoveryIsPartitionInvariant(t *testing.T) {
	wire := []byte("wat\x00buffer:c:2:a☃b\x00")
	for split := 0; split <= len(wire); split++ {
		decoder := testDecoder()
		var frames []DecodedFrame
		emit := func(frame DecodedFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 2 {
			t.Fatalf("split %d decoded %d frames, want 2", split, len(frames))
		}
		assertFrameKind(t, frames[0], FrameUnknownEvent)
		snapshot, ok := decodedEvent(t, frames[1]).Buffer()
		if !ok || snapshot.Cursor() != 4 || !bytes.Equal(snapshot.Bytes(), []byte("a☃b")) {
			t.Errorf("split %d recovered snapshot = (%s, %t)", split, snapshot, ok)
		}
	}
}

func TestDecoderOversizedFrameIsBoundedAndPartitionInvariant(t *testing.T) {
	wire := []byte("0123456789abcdef\x00prompt-ready\x00")
	for split := 0; split <= len(wire); split++ {
		decoder := NewDecoderWithFrameLimit(InitialStreamEpoch(), 12)
		var frames []DecodedFrame
		emit := func(frame DecodedFrame) { frames = append(frames, frame) }
		decoder.Push(wire[:split], emit)
		decoder.Push(wire[split:], emit)
		if len(frames) != 2 {
			t.Fatalf("split %d decoded %d frames, want 2", split, len(frames))
		}
		err := decodedError(t, frames[0])
		if err.Kind() != FrameTooLarge || err.ObservedBytes() != 16 || err.Limit() != 12 {
			t.Errorf("split %d oversized error = %#v", split, err)
		}
		if decodedEvent(t, frames[1]).Kind() != EventPromptReady {
			t.Errorf("split %d second frame is not prompt-ready", split)
		}
		if decoder.PendingLen() != 0 || decoder.Discarding() {
			t.Errorf("split %d decoder retained oversized state", split)
		}
	}

	decoder := NewDecoderWithFrameLimit(InitialStreamEpoch(), 12)
	decoder.Push([]byte("0123456789abcdef"), func(DecodedFrame) { t.Fatal("unterminated frame emitted") })
	if decoder.PendingLen() != 0 || !decoder.Discarding() {
		t.Errorf("oversized pending state = (%d, %t), want (0, true)", decoder.PendingLen(), decoder.Discarding())
	}
}

func TestDecoderStreamingCallbackRetainsNoResultBatch(t *testing.T) {
	decoder := NewDecoderWithFrameLimit(InitialStreamEpoch(), 8)
	input := make([]byte, 100_000)
	emitted := 0
	decoder.Push(input, func(DecodedFrame) { emitted++ })
	if emitted != len(input) || decoder.PendingLen() != 0 {
		t.Errorf("emitted %d with %d pending, want %d and 0", emitted, decoder.PendingLen(), len(input))
	}
}

func TestDecoderFinishAndReset(t *testing.T) {
	t.Run("truncated", func(t *testing.T) {
		decoder := testDecoder()
		decoder.Push([]byte("prompt"), func(DecodedFrame) { t.Fatal("unterminated frame emitted") })
		frame, ok := decoder.Finish()
		if !ok {
			t.Fatal("Finish() ok = false, want true")
		}
		err := decodedError(t, frame)
		if err.Kind() != FrameTruncated || err.ObservedBytes() != 6 {
			t.Errorf("Finish() error = %#v, want six-byte truncation", err)
		}
		if frame.Position().Sequence().Low() != 0 {
			t.Errorf("sequence = %d, want 0", frame.Position().Sequence().Low())
		}
		if _, ok := decoder.Finish(); ok {
			t.Error("second Finish() ok = true, want false")
		}
	})

	t.Run("oversized", func(t *testing.T) {
		decoder := NewDecoderWithFrameLimit(InitialStreamEpoch(), 2)
		decoder.Push([]byte("secret"), func(DecodedFrame) { t.Fatal("unterminated frame emitted") })
		frame, ok := decoder.Finish()
		if !ok {
			t.Fatal("Finish() ok = false, want true")
		}
		err := decodedError(t, frame)
		if err.Kind() != FrameTooLarge || err.ObservedBytes() != 6 || err.Limit() != 2 {
			t.Errorf("Finish() error = %#v", err)
		}
		if decoder.Discarding() {
			t.Error("Discarding() remains true after Finish")
		}
	})

	t.Run("reset", func(t *testing.T) {
		decoder := testDecoder()
		decoder.Push([]byte("secret"), func(DecodedFrame) { t.Fatal("partial frame emitted") })
		epoch, err := decoder.ResetStream()
		if err != nil || epoch.Value() != 1 {
			t.Fatalf("ResetStream() = (%d, %v), want (1, nil)", epoch.Value(), err)
		}
		frames := decodeEvents(decoder, []byte("prompt-ready\x00"))
		if frames[0].Position().Epoch() != epoch || frames[0].Position().Sequence().Low() != 0 {
			t.Errorf("position after reset = %v, want epoch 1 sequence 0", frames[0].Position())
		}
	})

	t.Run("epoch exhaustion", func(t *testing.T) {
		decoder := NewDecoder(StreamEpoch{value: math.MaxUint64})
		if _, err := decoder.ResetStream(); err == nil {
			t.Fatal("ResetStream() error = nil, want exhaustion")
		} else if resetErr, ok := err.(*StreamResetError); !ok || resetErr.Kind() != StreamResetEpochExhausted {
			t.Errorf("ResetStream() error = %T %v", err, err)
		}
	})
}

func TestDecoderFrameSequenceSaturatesAtUint128Maximum(t *testing.T) {
	decoder := testDecoder()
	decoder.nextSequence = FrameSequence{high: math.MaxUint64, low: math.MaxUint64 - 1}
	frames := decodeEvents(decoder, []byte("prompt-ready\x00prompt-ready\x00prompt-ready\x00"))
	want := []FrameSequence{
		{high: math.MaxUint64, low: math.MaxUint64 - 1},
		{high: math.MaxUint64, low: math.MaxUint64},
		{high: math.MaxUint64, low: math.MaxUint64},
	}
	for index, frame := range frames {
		if frame.Position().Sequence() != want[index] {
			t.Errorf("frame %d sequence = %v, want %v", index, frame.Position().Sequence(), want[index])
		}
	}
}

func TestDecoderFormattingRedactsPayloads(t *testing.T) {
	secret := "Dean Pelton's secret command"
	decoder := testDecoder()
	decoder.Push([]byte("buffer:b:0:"+secret), func(DecodedFrame) { t.Fatal("partial frame emitted") })
	frame := decodeEvents(testDecoder(), []byte("command-start:"+secret+"\x00"))[0]
	event := decodedEvent(t, frame)
	command, _ := event.Command()
	values := map[string]any{
		"decoder":       decoder,
		"decoder value": *decoder,
		"frame":         frame,
		"event":         event,
		"command":       command,
	}
	for name, value := range values {
		for _, format := range []string{"%v", "%+v", "%#v", "%s"} {
			if got := fmt.Sprintf(format, value); strings.Contains(got, secret) {
				t.Errorf("%s formatted with %q exposed payload: %s", name, format, got)
			}
		}
	}
}

func TestProbeResyncUsesSharedShellControlIdentifier(t *testing.T) {
	frames := decodeEvents(testDecoder(), []byte("probe-resync:2147483647:0\x00"))
	response, ok := decodedEvent(t, frames[0]).ProbeResync()
	if !ok || response.RequestID().Value() != shellcontrol.MaxProbeResyncRequestID {
		t.Errorf("request ID = %d, want shared maximum", response.RequestID().Value())
	}
}
