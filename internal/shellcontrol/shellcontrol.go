// Package shellcontrol encodes and incrementally decodes bounded parent-to-shell
// editing-buffer and probe-recovery controls.
package shellcontrol

import (
	"bytes"
	"fmt"
	"math"
	"strconv"
	"unicode/utf8"
)

const (
	// MaxControlBufferBytes is the largest decoded editing buffer accepted by
	// every shell adapter.
	MaxControlBufferBytes = 16 * 1024

	// MaxControlRequestID is the largest replacement identifier representable by
	// every shell adapter.
	MaxControlRequestID uint64 = 2_147_483_647

	// MaxProbeResyncRequestID is the largest probe-resynchronization identifier
	// supported by every shell adapter.
	MaxProbeResyncRequestID uint64 = 2_147_483_647

	replacementControlPrefix = "argmax-control-v1:replace:"
	probeResyncControlPrefix = "argmax-control-v1:resync:"
	maxRequestIDDigits       = 10
	maxBufferSizeDigits      = 5

	// MaxControlFrameBytes is the hard maximum frame size excluding the NUL
	// terminator.
	MaxControlFrameBytes = len(replacementControlPrefix) +
		maxRequestIDDigits + 1 +
		maxBufferSizeDigits + 1 +
		maxBufferSizeDigits + 1 +
		MaxControlBufferBytes*2

	// MaxControlWireBytes is the hard maximum control wire size including its
	// NUL terminator.
	MaxControlWireBytes = MaxControlFrameBytes + 1
)

// RequestIDErrorKind classifies an invalid control request identifier.
type RequestIDErrorKind uint8

const (
	// RequestIDZero means the supplied identifier was zero.
	RequestIDZero RequestIDErrorKind = iota + 1
	// RequestIDOutOfRange means the supplied identifier exceeded its domain.
	RequestIDOutOfRange
)

// ControlRequestIDError describes an invalid replacement-control identifier.
type ControlRequestIDError struct {
	kind RequestIDErrorKind
}

// Kind returns the identifier rejection classification.
func (e *ControlRequestIDError) Kind() RequestIDErrorKind { return e.kind }

// Maximum returns the largest accepted replacement identifier.
func (e *ControlRequestIDError) Maximum() uint64 { return MaxControlRequestID }

// Error returns a content-free description of the invalid identifier.
func (e *ControlRequestIDError) Error() string {
	if e.kind == RequestIDZero {
		return "shell control request identifier is zero"
	}
	return fmt.Sprintf(
		"shell control request identifier exceeds shared maximum %d",
		MaxControlRequestID,
	)
}

// GoString returns a structural representation of the error.
func (e *ControlRequestIDError) GoString() string {
	if e.kind == RequestIDZero {
		return "Zero"
	}
	return fmt.Sprintf("OutOfRange { maximum: %d }", MaxControlRequestID)
}

// ControlRequestID is a validated monotonic replacement-control identifier.
type ControlRequestID struct {
	value uint64
}

// NewControlRequestID validates a nonzero identifier supported by every shell
// adapter.
func NewControlRequestID(value uint64) (ControlRequestID, error) {
	switch {
	case value == 0:
		return ControlRequestID{}, &ControlRequestIDError{kind: RequestIDZero}
	case value > MaxControlRequestID:
		return ControlRequestID{}, &ControlRequestIDError{kind: RequestIDOutOfRange}
	default:
		return ControlRequestID{value: value}, nil
	}
}

// Value returns the numeric request identifier.
func (id ControlRequestID) Value() uint64 { return id.value }

// String returns the identifier's structural representation.
func (id ControlRequestID) String() string {
	return fmt.Sprintf("ControlRequestId(%d)", id.value)
}

// ProbeResyncRequestIDError describes an invalid probe-resynchronization
// request identifier.
type ProbeResyncRequestIDError struct {
	kind RequestIDErrorKind
}

// Kind returns the identifier rejection classification.
func (e *ProbeResyncRequestIDError) Kind() RequestIDErrorKind { return e.kind }

// Maximum returns the largest accepted probe-resynchronization identifier.
func (e *ProbeResyncRequestIDError) Maximum() uint64 { return MaxProbeResyncRequestID }

// Error returns a content-free description of the invalid identifier.
func (e *ProbeResyncRequestIDError) Error() string {
	if e.kind == RequestIDZero {
		return "probe resync request identifier is zero"
	}
	return fmt.Sprintf(
		"probe resync request identifier exceeds shared maximum %d",
		MaxProbeResyncRequestID,
	)
}

// GoString returns a structural representation of the error.
func (e *ProbeResyncRequestIDError) GoString() string {
	if e.kind == RequestIDZero {
		return "Zero"
	}
	return fmt.Sprintf("OutOfRange { maximum: %d }", MaxProbeResyncRequestID)
}

// ProbeResyncRequestID is a validated monotonic identifier for one explicit
// adapter-counter resynchronization. Its domain is independent of replacement
// control identifiers.
type ProbeResyncRequestID struct {
	value uint64
}

// NewProbeResyncRequestID validates a nonzero identifier supported by every
// shell adapter.
func NewProbeResyncRequestID(value uint64) (ProbeResyncRequestID, error) {
	switch {
	case value == 0:
		return ProbeResyncRequestID{}, &ProbeResyncRequestIDError{kind: RequestIDZero}
	case value > MaxProbeResyncRequestID:
		return ProbeResyncRequestID{}, &ProbeResyncRequestIDError{kind: RequestIDOutOfRange}
	default:
		return ProbeResyncRequestID{value: value}, nil
	}
}

// Value returns the numeric request identifier.
func (id ProbeResyncRequestID) Value() uint64 { return id.value }

// String returns the identifier's structural representation.
func (id ProbeResyncRequestID) String() string {
	return fmt.Sprintf("ProbeResyncRequestId(%d)", id.value)
}

// EncodeErrorKind classifies an invalid replacement supplied to the encoder.
type EncodeErrorKind uint8

const (
	// EncodeBufferTooLarge means the replacement exceeded the adapter-wide byte bound.
	EncodeBufferTooLarge EncodeErrorKind = iota + 1
	// EncodeNULBuffer means the replacement contained a NUL byte.
	EncodeNULBuffer
	// EncodeInvalidUTF8 means the replacement was not valid UTF-8.
	EncodeInvalidUTF8
	// EncodeInvalidCursor means the cursor was outside the buffer or split a rune.
	EncodeInvalidCursor
)

// EncodeError describes an invalid replacement without retaining its content.
type EncodeError struct {
	kind        EncodeErrorKind
	bytes       int
	limit       int
	cursor      int
	bufferBytes int
}

// Kind returns the replacement rejection classification.
func (e *EncodeError) Kind() EncodeErrorKind { return e.kind }

// Bytes returns the observed byte count for EncodeBufferTooLarge.
func (e *EncodeError) Bytes() int { return e.bytes }

// Limit returns the byte limit for EncodeBufferTooLarge.
func (e *EncodeError) Limit() int { return e.limit }

// Cursor returns the supplied cursor for EncodeInvalidCursor.
func (e *EncodeError) Cursor() int { return e.cursor }

// BufferBytes returns the replacement byte count for errors that report it.
func (e *EncodeError) BufferBytes() int { return e.bufferBytes }

// Error returns a description that never includes replacement content.
func (e *EncodeError) Error() string {
	switch e.kind {
	case EncodeBufferTooLarge:
		return fmt.Sprintf("shell control buffer is %d bytes; limit is %d", e.bytes, e.limit)
	case EncodeNULBuffer:
		return "shell control buffer contains NUL"
	case EncodeInvalidUTF8:
		return "shell control buffer is not UTF-8"
	case EncodeInvalidCursor:
		return fmt.Sprintf(
			"shell control cursor %d is invalid for %d bytes",
			e.cursor,
			e.bufferBytes,
		)
	default:
		return fmt.Sprintf("unknown shell control encode error (%d)", e.kind)
	}
}

// GoString returns a structural representation that never includes replacement
// content.
func (e *EncodeError) GoString() string {
	switch e.kind {
	case EncodeBufferTooLarge:
		return fmt.Sprintf("BufferTooLarge { bytes: %d, limit: %d }", e.bytes, e.limit)
	case EncodeNULBuffer:
		return "NulBuffer"
	case EncodeInvalidUTF8:
		return "InvalidUtf8"
	case EncodeInvalidCursor:
		return fmt.Sprintf(
			"InvalidCursor { cursor: %d, buffer_bytes: %d }",
			e.cursor,
			e.bufferBytes,
		)
	default:
		return fmt.Sprintf("EncodeErrorKind(%d)", e.kind)
	}
}

// ReplacementControl is one validated request to replace the shell-native
// editing buffer.
type ReplacementControl struct {
	requestID ControlRequestID
	buffer    string
	cursor    int
}

// NewReplacementControl validates an inert UTF-8 replacement and its byte cursor.
func NewReplacementControl(
	requestID ControlRequestID,
	buffer string,
	cursor int,
) (ReplacementControl, error) {
	switch {
	case requestID.value == 0:
		return ReplacementControl{}, &ControlRequestIDError{kind: RequestIDZero}
	case requestID.value > MaxControlRequestID:
		return ReplacementControl{}, &ControlRequestIDError{kind: RequestIDOutOfRange}
	}
	if err := validateReplacement(buffer, cursor); err != nil {
		return ReplacementControl{}, err
	}
	return ReplacementControl{requestID: requestID, buffer: buffer, cursor: cursor}, nil
}

// RequestID returns the correlated synchronization-probe identifier.
func (c ReplacementControl) RequestID() ControlRequestID { return c.requestID }

// Buffer returns the exact replacement text.
func (c ReplacementControl) Buffer() string { return c.buffer }

// Cursor returns the UTF-8 byte cursor within the replacement.
func (c ReplacementControl) Cursor() int { return c.cursor }

// Len returns the replacement size in bytes.
func (c ReplacementControl) Len() int { return len(c.buffer) }

// Empty reports whether the replacement is empty.
func (c ReplacementControl) Empty() bool { return c.buffer == "" }

// Encode encodes the request as one complete NUL-framed control.
func (c ReplacementControl) Encode() EncodedControlFrame {
	request := strconv.FormatUint(c.requestID.value, 10)
	cursor := strconv.Itoa(c.cursor)
	length := strconv.Itoa(len(c.buffer))
	capacity := len(replacementControlPrefix) + len(request) + len(cursor) +
		len(length) + len(c.buffer)*2 + 4
	wire := make([]byte, 0, capacity)
	wire = append(wire, replacementControlPrefix...)
	wire = append(wire, request...)
	wire = append(wire, ':')
	wire = append(wire, cursor...)
	wire = append(wire, ':')
	wire = append(wire, length...)
	wire = append(wire, ':')
	wire = appendLowerHex(wire, c.buffer)
	wire = append(wire, 0)
	return EncodedControlFrame{wire: wire}
}

// String returns a structural representation that redacts replacement content.
func (c ReplacementControl) String() string {
	return fmt.Sprintf(
		"ReplacementControl { request_id: %s, buffer_bytes: %d, cursor: %d }",
		c.requestID,
		len(c.buffer),
		c.cursor,
	)
}

// GoString returns a structural representation that redacts replacement content.
func (c ReplacementControl) GoString() string { return c.String() }

// ProbeResyncControl requests the shell adapter's current ordinary probe counter.
type ProbeResyncControl struct {
	requestID ProbeResyncRequestID
}

// NewProbeResyncControl creates a correlated adapter-counter resynchronization
// request.
func NewProbeResyncControl(requestID ProbeResyncRequestID) ProbeResyncControl {
	return ProbeResyncControl{requestID: requestID}
}

// RequestID returns the independent resynchronization request identifier.
func (c ProbeResyncControl) RequestID() ProbeResyncRequestID { return c.requestID }

// Encode encodes the request as one complete NUL-framed control.
func (c ProbeResyncControl) Encode() EncodedControlFrame {
	request := strconv.FormatUint(c.requestID.value, 10)
	wire := make([]byte, 0, len(probeResyncControlPrefix)+len(request)+1)
	wire = append(wire, probeResyncControlPrefix...)
	wire = append(wire, request...)
	wire = append(wire, 0)
	return EncodedControlFrame{wire: wire}
}

// String returns the control's structural representation.
func (c ProbeResyncControl) String() string {
	return fmt.Sprintf("ProbeResyncControl { request_id: %s }", c.requestID)
}

// GoString returns the control's structural representation.
func (c ProbeResyncControl) GoString() string { return c.String() }

// EncodedControlFrame contains complete parent-to-shell control wire bytes,
// including the NUL terminator.
type EncodedControlFrame struct {
	wire []byte
}

// Bytes returns a copy of the exact bytes to queue on the private control stream.
func (f EncodedControlFrame) Bytes() []byte { return bytes.Clone(f.wire) }

// Len returns the complete framed size.
func (f EncodedControlFrame) Len() int { return len(f.wire) }

// Empty reports whether the encoded frame is empty. Valid frames are never empty.
func (f EncodedControlFrame) Empty() bool { return len(f.wire) == 0 }

// String returns a structural representation that redacts wire content.
func (f EncodedControlFrame) String() string {
	return fmt.Sprintf("EncodedControlFrame { wire_bytes: %d }", len(f.wire))
}

// GoString returns a structural representation that redacts wire content.
func (f EncodedControlFrame) GoString() string { return f.String() }

// FrameErrorKind classifies a rejected control frame.
type FrameErrorKind uint8

const (
	// FrameEmpty means a terminator appeared without frame bytes.
	FrameEmpty FrameErrorKind = iota + 1
	// FrameWrongDirection means the frame was not a parent-to-shell control.
	FrameWrongDirection
	// FrameUnsupportedProtocol means the version or operation was unsupported.
	FrameUnsupportedProtocol
	// FrameInvalidGrammar means required fields were missing or duplicated.
	FrameInvalidGrammar
	// FrameInvalidRequestID means the identifier was not canonical unsigned decimal.
	FrameInvalidRequestID
	// FrameRequestIDOutOfRange means the identifier was outside the shared domain.
	FrameRequestIDOutOfRange
	// FrameInvalidCursor means the cursor was not canonical unsigned decimal.
	FrameInvalidCursor
	// FrameInvalidLength means the length was not canonical unsigned decimal.
	FrameInvalidLength
	// FrameBufferTooLarge means the declared replacement exceeded the byte bound.
	FrameBufferTooLarge
	// FrameHexLengthMismatch means payload size differed from the declared byte count.
	FrameHexLengthMismatch
	// FrameInvalidHex means the payload was not lowercase hexadecimal ASCII.
	FrameInvalidHex
	// FrameNULBuffer means the decoded replacement contained NUL.
	FrameNULBuffer
	// FrameInvalidUTF8 means the decoded replacement was not UTF-8.
	FrameInvalidUTF8
	// FrameCursorOutOfRange means the cursor exceeded the decoded replacement.
	FrameCursorOutOfRange
	// FrameCursorNotUTF8Boundary means the cursor split a UTF-8 encoding.
	FrameCursorNotUTF8Boundary
	// FrameTooLarge means an unterminated frame exceeded the decoder limit.
	FrameTooLarge
	// FrameTruncated means input ended before a NUL terminator.
	FrameTruncated
)

// FrameError is a closed, content-free reason a control frame was rejected.
type FrameError struct {
	kind          FrameErrorKind
	observedBytes int
	bytes         uint64
	limit         int
}

// Kind returns the frame rejection classification.
func (e *FrameError) Kind() FrameErrorKind { return e.kind }

// ObservedBytes returns the retained or discarded frame count for FrameTooLarge
// and FrameTruncated.
func (e *FrameError) ObservedBytes() int { return e.observedBytes }

// Bytes returns the declared replacement byte count for FrameBufferTooLarge.
func (e *FrameError) Bytes() uint64 { return e.bytes }

// Limit returns the applicable frame or replacement byte limit.
func (e *FrameError) Limit() int { return e.limit }

// Error returns a content-free frame rejection description.
func (e *FrameError) Error() string {
	switch e.kind {
	case FrameEmpty:
		return "empty shell control frame"
	case FrameWrongDirection:
		return "shell control has the wrong direction"
	case FrameUnsupportedProtocol:
		return "unsupported shell control version or operation"
	case FrameInvalidGrammar:
		return "invalid shell control grammar"
	case FrameInvalidRequestID:
		return "shell control request identifier is not canonical decimal"
	case FrameRequestIDOutOfRange:
		return "shell control request identifier is out of range"
	case FrameInvalidCursor:
		return "shell control cursor is not canonical decimal"
	case FrameInvalidLength:
		return "shell control length is not canonical decimal"
	case FrameBufferTooLarge:
		return fmt.Sprintf("shell control buffer is %d bytes; limit is %d", e.bytes, e.limit)
	case FrameHexLengthMismatch:
		return "shell control hex length does not match its declaration"
	case FrameInvalidHex:
		return "shell control payload is not lowercase hexadecimal"
	case FrameNULBuffer:
		return "shell control buffer contains NUL"
	case FrameInvalidUTF8:
		return "shell control buffer is not UTF-8"
	case FrameCursorOutOfRange:
		return "shell control cursor is out of range"
	case FrameCursorNotUTF8Boundary:
		return "shell control cursor splits a UTF-8 scalar"
	case FrameTooLarge:
		return fmt.Sprintf(
			"shell control frame is %d bytes; limit is %d",
			e.observedBytes,
			e.limit,
		)
	case FrameTruncated:
		return fmt.Sprintf(
			"shell control frame ended after %d bytes without a terminator",
			e.observedBytes,
		)
	default:
		return fmt.Sprintf("unknown shell control frame error (%d)", e.kind)
	}
}

// GoString returns the structural frame rejection reason.
func (e *FrameError) GoString() string {
	switch e.kind {
	case FrameEmpty:
		return "EmptyFrame"
	case FrameWrongDirection:
		return "WrongDirection"
	case FrameUnsupportedProtocol:
		return "UnsupportedProtocol"
	case FrameInvalidGrammar:
		return "InvalidGrammar"
	case FrameInvalidRequestID:
		return "InvalidRequestId"
	case FrameRequestIDOutOfRange:
		return "RequestIdOutOfRange"
	case FrameInvalidCursor:
		return "InvalidCursor"
	case FrameInvalidLength:
		return "InvalidLength"
	case FrameBufferTooLarge:
		return fmt.Sprintf("BufferTooLarge { bytes: %d, limit: %d }", e.bytes, e.limit)
	case FrameHexLengthMismatch:
		return "HexLengthMismatch"
	case FrameInvalidHex:
		return "InvalidHex"
	case FrameNULBuffer:
		return "NulBuffer"
	case FrameInvalidUTF8:
		return "InvalidUtf8"
	case FrameCursorOutOfRange:
		return "CursorOutOfRange"
	case FrameCursorNotUTF8Boundary:
		return "CursorNotUtf8Boundary"
	case FrameTooLarge:
		return fmt.Sprintf(
			"FrameTooLarge { observed_bytes: %d, limit: %d }",
			e.observedBytes,
			e.limit,
		)
	case FrameTruncated:
		return fmt.Sprintf("TruncatedFrame { observed_bytes: %d }", e.observedBytes)
	default:
		return fmt.Sprintf("FrameErrorKind(%d)", e.kind)
	}
}

// DecodedFrameKind identifies one complete control-stream result.
type DecodedFrameKind uint8

const (
	// DecodedReplacement identifies a validated inert replacement request.
	DecodedReplacement DecodedFrameKind = iota + 1
	// DecodedProbeResync identifies a validated adapter-counter request.
	DecodedProbeResync
	// DecodedRejected identifies one isolated malformed frame.
	DecodedRejected
)

// DecodedControlFrame is one complete result from the parent-to-shell control
// stream.
type DecodedControlFrame struct {
	kind        DecodedFrameKind
	replacement ReplacementControl
	probeResync ProbeResyncControl
	err         *FrameError
}

// Kind returns the decoded result classification.
func (f DecodedControlFrame) Kind() DecodedFrameKind { return f.kind }

// Replacement returns the decoded replacement when Kind is DecodedReplacement.
func (f DecodedControlFrame) Replacement() (ReplacementControl, bool) {
	return f.replacement, f.kind == DecodedReplacement
}

// ProbeResync returns the decoded request when Kind is DecodedProbeResync.
func (f DecodedControlFrame) ProbeResync() (ProbeResyncControl, bool) {
	return f.probeResync, f.kind == DecodedProbeResync
}

// Err returns the rejection reason when Kind is DecodedRejected and nil otherwise.
func (f DecodedControlFrame) Err() error {
	if f.kind != DecodedRejected {
		return nil
	}
	return f.err
}

// String returns a structural representation that redacts replacement content.
func (f DecodedControlFrame) String() string {
	switch f.kind {
	case DecodedReplacement:
		return fmt.Sprintf("Replacement(%s)", f.replacement)
	case DecodedProbeResync:
		return fmt.Sprintf("ProbeResync(%s)", f.probeResync)
	case DecodedRejected:
		return fmt.Sprintf("Rejected(%#v)", f.err)
	default:
		return fmt.Sprintf("DecodedControlFrame(%d)", f.kind)
	}
}

// GoString returns a structural representation that redacts replacement content.
func (f DecodedControlFrame) GoString() string { return f.String() }

// Decoder incrementally decodes NUL-framed controls with bounded retained storage.
type Decoder struct {
	pending       []byte
	oversized     bool
	oversizedSize int
	frameLimit    int
}

// NewDecoder creates a decoder at the hard protocol frame limit.
func NewDecoder() *Decoder { return NewDecoderWithFrameLimit(MaxControlFrameBytes) }

// NewDecoderWithFrameLimit creates a decoder with a caller limit capped by the
// protocol maximum. A zero limit accepts only empty frames.
func NewDecoderWithFrameLimit(frameLimit int) *Decoder {
	if frameLimit < 0 {
		frameLimit = 0
	}
	frameLimit = min(frameLimit, MaxControlFrameBytes)
	return &Decoder{
		pending:    make([]byte, 0, min(frameLimit, 4*1024)),
		frameLimit: frameLimit,
	}
}

// FrameLimit returns the configured maximum retained bytes per unterminated frame.
func (d *Decoder) FrameLimit() int { return d.frameLimit }

// PendingLen returns the number of bytes retained for a partial frame.
func (d *Decoder) PendingLen() int { return len(d.pending) }

// Discarding reports whether an oversized frame is being discarded through its
// terminator.
func (d *Decoder) Discarding() bool { return d.oversized }

// Push consumes a stream chunk and emits complete results in wire order.
func (d *Decoder) Push(chunk []byte, emit func(DecodedControlFrame)) {
	for _, char := range chunk {
		if char == 0 {
			emit(d.finishTerminatedFrame())
			continue
		}
		if d.oversized {
			if d.oversizedSize < math.MaxInt {
				d.oversizedSize++
			}
			continue
		}
		if len(d.pending) < d.frameLimit {
			d.pending = append(d.pending, char)
			continue
		}
		d.oversized = true
		d.oversizedSize = saturatedIncrement(len(d.pending))
		clear(d.pending)
		d.pending = d.pending[:0]
	}
}

// Finish rejects one partial frame at end of input and clears decoder state.
// The boolean is false when no frame is pending.
func (d *Decoder) Finish() (DecodedControlFrame, bool) {
	if d.oversized {
		frame := rejectedFrame(&FrameError{
			kind:          FrameTooLarge,
			observedBytes: d.oversizedSize,
			limit:         d.frameLimit,
		})
		d.oversized = false
		d.oversizedSize = 0
		return frame, true
	}
	if len(d.pending) == 0 {
		return DecodedControlFrame{}, false
	}
	observedBytes := len(d.pending)
	clear(d.pending)
	d.pending = d.pending[:0]
	return rejectedFrame(&FrameError{
		kind:          FrameTruncated,
		observedBytes: observedBytes,
	}), true
}

// String returns a structural representation that redacts pending frame content.
func (d *Decoder) String() string {
	oversized := "None"
	if d.oversized {
		oversized = fmt.Sprintf("Some(%d)", d.oversizedSize)
	}
	return fmt.Sprintf(
		"ShellControlDecoder { pending_bytes: %d, oversized_bytes: %s, frame_limit: %d }",
		len(d.pending),
		oversized,
		d.frameLimit,
	)
}

// GoString returns a structural representation that redacts pending frame content.
func (d *Decoder) GoString() string { return d.String() }

func (d *Decoder) finishTerminatedFrame() DecodedControlFrame {
	if d.oversized {
		frame := rejectedFrame(&FrameError{
			kind:          FrameTooLarge,
			observedBytes: d.oversizedSize,
			limit:         d.frameLimit,
		})
		d.oversized = false
		d.oversizedSize = 0
		return frame
	}
	frame := parseControlFrame(d.pending)
	clear(d.pending)
	d.pending = d.pending[:0]
	return frame
}

func validateReplacement(buffer string, cursor int) error {
	if len(buffer) > MaxControlBufferBytes {
		return &EncodeError{
			kind:  EncodeBufferTooLarge,
			bytes: len(buffer),
			limit: MaxControlBufferBytes,
		}
	}
	if bytes.IndexByte([]byte(buffer), 0) >= 0 {
		return &EncodeError{kind: EncodeNULBuffer}
	}
	if !utf8.ValidString(buffer) {
		return &EncodeError{kind: EncodeInvalidUTF8, bufferBytes: len(buffer)}
	}
	if cursor < 0 || cursor > len(buffer) || !utf8.ValidString(buffer[:cursor]) {
		return &EncodeError{
			kind:        EncodeInvalidCursor,
			cursor:      cursor,
			bufferBytes: len(buffer),
		}
	}
	return nil
}

func appendLowerHex(destination []byte, source string) []byte {
	const digits = "0123456789abcdef"
	for i := range len(source) {
		char := source[i]
		destination = append(destination, digits[char>>4], digits[char&0x0f])
	}
	return destination
}

func parseControlFrame(frame []byte) DecodedControlFrame {
	if len(frame) == 0 {
		return rejectedKind(FrameEmpty)
	}
	if request, ok := bytes.CutPrefix(frame, []byte(probeResyncControlPrefix)); ok {
		if bytes.IndexByte(request, ':') >= 0 {
			return rejectedKind(FrameInvalidGrammar)
		}
		value, ok := parseDecimalUint64(request)
		if !ok {
			return rejectedKind(FrameInvalidRequestID)
		}
		requestID, err := NewProbeResyncRequestID(value)
		if err != nil {
			return rejectedKind(FrameRequestIDOutOfRange)
		}
		return DecodedControlFrame{
			kind:        DecodedProbeResync,
			probeResync: NewProbeResyncControl(requestID),
		}
	}

	fields, ok := bytes.CutPrefix(frame, []byte(replacementControlPrefix))
	if !ok {
		if bytes.HasPrefix(frame, []byte("argmax-control-")) {
			return rejectedKind(FrameUnsupportedProtocol)
		}
		return rejectedKind(FrameWrongDirection)
	}
	parts := bytes.Split(fields, []byte{':'})
	if len(parts) != 4 {
		return rejectedKind(FrameInvalidGrammar)
	}

	requestValue, ok := parseDecimalUint64(parts[0])
	if !ok {
		return rejectedKind(FrameInvalidRequestID)
	}
	requestID, err := NewControlRequestID(requestValue)
	if err != nil {
		return rejectedKind(FrameRequestIDOutOfRange)
	}
	cursorValue, ok := parseDecimalUint64(parts[1])
	if !ok {
		return rejectedKind(FrameInvalidCursor)
	}
	lengthValue, ok := parseDecimalUint64(parts[2])
	if !ok {
		return rejectedKind(FrameInvalidLength)
	}
	if lengthValue > MaxControlBufferBytes {
		return rejectedFrame(&FrameError{
			kind:  FrameBufferTooLarge,
			bytes: lengthValue,
			limit: MaxControlBufferBytes,
		})
	}
	length := int(lengthValue)
	hexPayload := parts[3]
	if len(hexPayload) != length*2 {
		return rejectedKind(FrameHexLengthMismatch)
	}
	decoded := make([]byte, length)
	for i := range length {
		high, ok := decodeLowerHexDigit(hexPayload[i*2])
		if !ok {
			return rejectedKind(FrameInvalidHex)
		}
		low, ok := decodeLowerHexDigit(hexPayload[i*2+1])
		if !ok {
			return rejectedKind(FrameInvalidHex)
		}
		decoded[i] = high<<4 | low
	}
	if bytes.IndexByte(decoded, 0) >= 0 {
		return rejectedKind(FrameNULBuffer)
	}
	if !utf8.Valid(decoded) {
		return rejectedKind(FrameInvalidUTF8)
	}
	if cursorValue > uint64(len(decoded)) {
		return rejectedKind(FrameCursorOutOfRange)
	}
	cursor := int(cursorValue)
	if !utf8.Valid(decoded[:cursor]) {
		return rejectedKind(FrameCursorNotUTF8Boundary)
	}
	return DecodedControlFrame{
		kind: DecodedReplacement,
		replacement: ReplacementControl{
			requestID: requestID,
			buffer:    string(decoded),
			cursor:    cursor,
		},
	}
}

func rejectedKind(kind FrameErrorKind) DecodedControlFrame {
	return rejectedFrame(&FrameError{kind: kind})
}

func rejectedFrame(err *FrameError) DecodedControlFrame {
	return DecodedControlFrame{kind: DecodedRejected, err: err}
}

func parseDecimalUint64(value []byte) (uint64, bool) {
	if !isCanonicalDecimal(value) {
		return 0, false
	}
	parsed, err := strconv.ParseUint(string(value), 10, 64)
	return parsed, err == nil
}

func isCanonicalDecimal(value []byte) bool {
	if len(value) == 0 || len(value) > 1 && value[0] == '0' {
		return false
	}
	for _, char := range value {
		if char < '0' || char > '9' {
			return false
		}
	}
	return true
}

func decodeLowerHexDigit(char byte) (byte, bool) {
	switch {
	case char >= '0' && char <= '9':
		return char - '0', true
	case char >= 'a' && char <= 'f':
		return char - 'a' + 10, true
	default:
		return 0, false
	}
}

func saturatedIncrement(value int) int {
	if value == math.MaxInt {
		return value
	}
	return value + 1
}
