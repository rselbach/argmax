// Package shellevents incrementally decodes bounded shell-integration events
// and reduces them into authoritative shell session state.
package shellevents

import (
	"bytes"
	"fmt"
	"math"
	"strconv"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/shellcontrol"
)

const (
	// MaxFrameBytes is the hard maximum size of one frame, excluding its NUL
	// terminator.
	MaxFrameBytes = 256 * 1024

	// SyncProbeSequence is the reserved input sequence that asks a shell-native
	// editor for a snapshot. It is a string constant so callers cannot mutate it.
	SyncProbeSequence = "\x1b[argmax-sync~"

	maxWorkingDirectoryBytes = 16 * 1024
	reloadRequestPrefix      = "reload-request:"
)

var (
	bufferBytePrefix           = []byte("buffer:b:")
	bufferCharacterPrefix      = []byte("buffer:c:")
	probeBufferBytePrefix      = []byte("probe-buffer:b:")
	probeBufferCharacterPrefix = []byte("probe-buffer:c:")
	probeBufferFishPrefix      = []byte("probe-buffer:f:")
	probeResyncPrefix          = []byte("probe-resync:")
	capabilityNative           = []byte("capability:native-buffer")
	capabilityProbePrefix      = []byte("capability:sync-probe:")
	capabilityUnavailable      = []byte("capability:unavailable")
	commandStartPrefix         = []byte("command-start:")
	commandStartUnknown        = []byte("command-start-unknown")
	commandStopPrefix          = []byte("command-stop:")
	workingDirectoryPrefix     = []byte("cwd:")
	promptReady                = []byte("prompt-ready")
)

// SubmittedCommand contains exact submitted command bytes reported by a shell
// lifecycle hook.
type SubmittedCommand struct {
	bytes []byte
}

// Bytes returns a copy of the command bytes without lossy decoding.
func (c SubmittedCommand) Bytes() []byte { return bytes.Clone(c.bytes) }

// StringValue returns the command as text only when its bytes are valid UTF-8.
func (c SubmittedCommand) StringValue() (string, bool) {
	if !utf8.Valid(c.bytes) {
		return "", false
	}
	return string(c.bytes), true
}

// Len returns the command length in bytes.
func (c SubmittedCommand) Len() int { return len(c.bytes) }

// Empty reports whether the submitted command is empty.
func (c SubmittedCommand) Empty() bool { return len(c.bytes) == 0 }

// String returns a representation that redacts command content.
func (c SubmittedCommand) String() string {
	return fmt.Sprintf("SubmittedCommand { byte_count: %d }", len(c.bytes))
}

// GoString returns a representation that redacts command content.
func (c SubmittedCommand) GoString() string { return c.String() }

// SnapshotNonce is a monotonic nonce echoed by a reserved synchronization
// probe.
type SnapshotNonce struct {
	value uint64
}

// Value returns the numeric nonce.
func (n SnapshotNonce) Value() uint64 { return n.value }

// String returns the nonce's structural representation.
func (n SnapshotNonce) String() string { return fmt.Sprintf("SnapshotNonce(%d)", n.value) }

// ProbeResyncResponse is a correlated report of a shell adapter's current
// ordinary probe counter.
type ProbeResyncResponse struct {
	requestID      shellcontrol.ProbeResyncRequestID
	lastProbeNonce SnapshotNonce
}

// RequestID returns the independent request identifier echoed by the adapter.
func (r ProbeResyncResponse) RequestID() shellcontrol.ProbeResyncRequestID {
	return r.requestID
}

// LastProbeNonce returns the adapter's current ordinary probe counter.
func (r ProbeResyncResponse) LastProbeNonce() SnapshotNonce { return r.lastProbeNonce }

// BufferSnapshot is a validated cursor-bearing editing snapshot. It becomes
// authoritative only after ShellSessionState correlates it with current state.
type BufferSnapshot struct {
	bytes      []byte
	cursor     int
	probeNonce SnapshotNonce
	hasProbe   bool
}

func emptyBufferSnapshot() BufferSnapshot { return BufferSnapshot{} }

// Bytes returns a copy of the exact buffer bytes.
func (s BufferSnapshot) Bytes() []byte { return bytes.Clone(s.bytes) }

// StringValue returns the buffer as text when valid UTF-8. Decoded snapshots
// are always valid UTF-8.
func (s BufferSnapshot) StringValue() (string, bool) {
	if !utf8.Valid(s.bytes) {
		return "", false
	}
	return string(s.bytes), true
}

// Cursor returns the validated UTF-8 byte offset of the cursor.
func (s BufferSnapshot) Cursor() int { return s.cursor }

// ProbeNonce returns the correlated probe nonce and whether one is present.
func (s BufferSnapshot) ProbeNonce() (SnapshotNonce, bool) {
	return s.probeNonce, s.hasProbe
}

// BeforeCursor returns a copy of the exact bytes before the cursor.
func (s BufferSnapshot) BeforeCursor() []byte { return bytes.Clone(s.bytes[:s.cursor]) }

// AfterCursor returns a copy of the exact bytes at and after the cursor.
func (s BufferSnapshot) AfterCursor() []byte { return bytes.Clone(s.bytes[s.cursor:]) }

// Len returns the buffer length in bytes.
func (s BufferSnapshot) Len() int { return len(s.bytes) }

// Empty reports whether the buffer is empty.
func (s BufferSnapshot) Empty() bool { return len(s.bytes) == 0 }

// String returns a representation that redacts buffer content.
func (s BufferSnapshot) String() string {
	probe := "None"
	if s.hasProbe {
		probe = fmt.Sprintf("Some(%s)", s.probeNonce)
	}
	return fmt.Sprintf(
		"BufferSnapshot { byte_count: %d, cursor: %d, probe_nonce: %s }",
		len(s.bytes), s.cursor, probe,
	)
}

// GoString returns a representation that redacts buffer content.
func (s BufferSnapshot) GoString() string { return s.String() }

// ShellExitStatus is a validated status in the portable 8-bit range.
type ShellExitStatus struct {
	value uint8
}

// Value returns the numeric shell status.
func (s ShellExitStatus) Value() uint8 { return s.value }

// Success reports whether the command completed successfully.
func (s ShellExitStatus) Success() bool { return s.value == 0 }

// ShellWorkingDirectory is a validated bounded absolute Unix path.
type ShellWorkingDirectory struct {
	bytes []byte
}

// Bytes returns a copy of the exact path bytes without lossy decoding.
func (d ShellWorkingDirectory) Bytes() []byte { return bytes.Clone(d.bytes) }

// String returns a representation that redacts path content.
func (d ShellWorkingDirectory) String() string {
	return fmt.Sprintf("ShellWorkingDirectory { path_bytes: %d }", len(d.bytes))
}

// GoString returns a representation that redacts path content.
func (d ShellWorkingDirectory) GoString() string { return d.String() }

// ReloadRequest carries the correlation nonce of an explicit active-session
// reload request.
type ReloadRequest struct {
	nonce uint32
}

// Nonce returns the request nonce echoed by the wrapper acknowledgment.
func (r ReloadRequest) Nonce() uint32 { return r.nonce }

// BufferSyncCapability describes runtime shell-adapter support for
// authoritative editing snapshots.
type BufferSyncCapability uint8

const (
	// BufferSyncUnknown means capability has not yet been announced.
	BufferSyncUnknown BufferSyncCapability = iota
	// BufferSyncNative means the shell reports native redraw callbacks without
	// wire correlation.
	BufferSyncNative
	// BufferSyncProbe means the reserved probe is bound without replacing a user binding.
	BufferSyncProbe
	// BufferSyncUnavailable means no safe callback or probe binding is available.
	BufferSyncUnavailable
)

// SupportsLiveSnapshots reports whether the adapter can provide authoritative
// live snapshots.
func (c BufferSyncCapability) SupportsLiveSnapshots() bool {
	return c == BufferSyncNative || c == BufferSyncProbe
}

// CapabilityAnnouncement is one fresh runtime capability handshake for a
// decoder epoch.
type CapabilityAnnouncement struct {
	capability     BufferSyncCapability
	lastProbeNonce SnapshotNonce
	hasLastNonce   bool
}

// Capability returns the announced synchronization capability.
func (a CapabilityAnnouncement) Capability() BufferSyncCapability { return a.capability }

// LastProbeNonce returns the probe counter base and whether one was announced.
func (a CapabilityAnnouncement) LastProbeNonce() (SnapshotNonce, bool) {
	return a.lastProbeNonce, a.hasLastNonce
}

// ShellEventKind identifies one syntactically and semantically validated event.
type ShellEventKind uint8

const (
	// EventBuffer identifies a cursor-bearing editing snapshot.
	EventBuffer ShellEventKind = iota + 1
	// EventPromptReady identifies an empty editable prompt boundary.
	EventPromptReady
	// EventCommandStart identifies a command start with exact submitted bytes.
	EventCommandStart
	// EventCommandStartUnknown identifies a start without exact submitted bytes.
	EventCommandStartUnknown
	// EventCommandStop identifies a foreground command stop.
	EventCommandStop
	// EventCapability identifies a runtime capability announcement.
	EventCapability
	// EventProbeResync identifies a correlated probe-counter response.
	EventProbeResync
	// EventWorkingDirectory identifies an absolute prompt working directory.
	EventWorkingDirectory
	// EventReloadRequest identifies a correlated live reload request.
	EventReloadRequest
)

// ShellEvent is one validated shell event.
type ShellEvent struct {
	kind          ShellEventKind
	buffer        BufferSnapshot
	command       SubmittedCommand
	status        ShellExitStatus
	capability    CapabilityAnnouncement
	probeResync   ProbeResyncResponse
	directory     ShellWorkingDirectory
	reloadRequest ReloadRequest
}

// Kind returns the event variant.
func (e ShellEvent) Kind() ShellEventKind { return e.kind }

// Buffer returns the snapshot when Kind is EventBuffer.
func (e ShellEvent) Buffer() (BufferSnapshot, bool) { return e.buffer, e.kind == EventBuffer }

// Command returns the submitted command when Kind is EventCommandStart.
func (e ShellEvent) Command() (SubmittedCommand, bool) {
	return e.command, e.kind == EventCommandStart
}

// Status returns the status when Kind is EventCommandStop.
func (e ShellEvent) Status() (ShellExitStatus, bool) { return e.status, e.kind == EventCommandStop }

// Capability returns the announcement when Kind is EventCapability.
func (e ShellEvent) Capability() (CapabilityAnnouncement, bool) {
	return e.capability, e.kind == EventCapability
}

// ProbeResync returns the response when Kind is EventProbeResync.
func (e ShellEvent) ProbeResync() (ProbeResyncResponse, bool) {
	return e.probeResync, e.kind == EventProbeResync
}

// WorkingDirectory returns the directory when Kind is EventWorkingDirectory.
func (e ShellEvent) WorkingDirectory() (ShellWorkingDirectory, bool) {
	return e.directory, e.kind == EventWorkingDirectory
}

// ReloadRequest returns the request when Kind is EventReloadRequest.
func (e ShellEvent) ReloadRequest() (ReloadRequest, bool) {
	return e.reloadRequest, e.kind == EventReloadRequest
}

// String returns a structural representation that redacts payload content.
func (e ShellEvent) String() string {
	switch e.kind {
	case EventBuffer:
		return fmt.Sprintf("Buffer(%s)", e.buffer)
	case EventPromptReady:
		return "PromptReady"
	case EventCommandStart:
		return fmt.Sprintf("CommandStart(%s)", e.command)
	case EventCommandStartUnknown:
		return "CommandStartUnknown"
	case EventCommandStop:
		return fmt.Sprintf("CommandStop(%d)", e.status.value)
	case EventCapability:
		return fmt.Sprintf("Capability(%d)", e.capability.capability)
	case EventProbeResync:
		return fmt.Sprintf("ProbeResync(%s,%s)", e.probeResync.requestID, e.probeResync.lastProbeNonce)
	case EventWorkingDirectory:
		return fmt.Sprintf("WorkingDirectory(%s)", e.directory)
	case EventReloadRequest:
		return fmt.Sprintf("ReloadRequest(%d)", e.reloadRequest.nonce)
	default:
		return fmt.Sprintf("ShellEvent(%d)", e.kind)
	}
}

// GoString returns a structural representation that redacts payload content.
func (e ShellEvent) GoString() string { return e.String() }

// StreamEpoch is a monotonic identifier for one decoder stream lifetime.
type StreamEpoch struct {
	value uint64
}

// InitialStreamEpoch returns the epoch for the first decoder in a session.
func InitialStreamEpoch() StreamEpoch { return StreamEpoch{} }

// Value returns the numeric epoch.
func (e StreamEpoch) Value() uint64 { return e.value }

// Next returns the next epoch and false after numeric exhaustion.
func (e StreamEpoch) Next() (StreamEpoch, bool) {
	if e.value == math.MaxUint64 {
		return StreamEpoch{}, false
	}
	return StreamEpoch{value: e.value + 1}, true
}

// FrameSequence is an exact unsigned 128-bit frame sequence represented as two
// immutable 64-bit words.
type FrameSequence struct {
	high uint64
	low  uint64
}

// High returns the high 64 bits.
func (s FrameSequence) High() uint64 { return s.high }

// Low returns the low 64 bits.
func (s FrameSequence) Low() uint64 { return s.low }

func (s FrameSequence) nextSaturating() FrameSequence {
	if s.low != math.MaxUint64 {
		return FrameSequence{high: s.high, low: s.low + 1}
	}
	if s.high != math.MaxUint64 {
		return FrameSequence{high: s.high + 1}
	}
	return s
}

func (s FrameSequence) checkedNext() (FrameSequence, bool) {
	if s.high == math.MaxUint64 && s.low == math.MaxUint64 {
		return FrameSequence{}, false
	}
	return s.nextSaturating(), true
}

// FramePosition is the unique position of a frame within a session stream.
type FramePosition struct {
	epoch    StreamEpoch
	sequence FrameSequence
}

// Epoch returns the stream epoch.
func (p FramePosition) Epoch() StreamEpoch { return p.epoch }

// Sequence returns the exact unsigned 128-bit sequence within the epoch.
func (p FramePosition) Sequence() FrameSequence { return p.sequence }

// InputGeneration identifies editing authority advanced before each locally
// forwarded editing input.
type InputGeneration struct {
	epoch    StreamEpoch
	sequence uint64
}

// Epoch returns the stream epoch in which this generation was issued.
func (g InputGeneration) Epoch() StreamEpoch { return g.epoch }

// Sequence returns the local-input sequence within the epoch.
func (g InputGeneration) Sequence() uint64 { return g.sequence }

// SequencedShellEvent pairs a valid event with its stream position.
type SequencedShellEvent struct {
	position FramePosition
	event    ShellEvent
}

// Position returns the event's stream position.
func (e SequencedShellEvent) Position() FramePosition { return e.position }

// Event returns the decoded event.
func (e SequencedShellEvent) Event() ShellEvent { return e.event }

// String returns a structural representation that redacts event payloads.
func (e SequencedShellEvent) String() string {
	return fmt.Sprintf("SequencedShellEvent { position: %v, event: %s }", e.position, e.event)
}

// GoString returns a structural representation that redacts event payloads.
func (e SequencedShellEvent) GoString() string { return e.String() }

// FrameErrorKind classifies a complete rejected protocol frame.
type FrameErrorKind uint8

const (
	// FrameEmpty means the frame contained no bytes.
	FrameEmpty FrameErrorKind = iota + 1
	// FrameUnknownEvent means the event name was unrecognized.
	FrameUnknownEvent
	// FrameMissingCursor means a cursor or separator was absent.
	FrameMissingCursor
	// FrameNonDecimalCursor means a cursor was not ASCII decimal.
	FrameNonDecimalCursor
	// FrameCursorOutOfRange means a cursor exceeded its buffer.
	FrameCursorOutOfRange
	// FrameCursorNotUTF8Boundary means a byte cursor split a UTF-8 code point.
	FrameCursorNotUTF8Boundary
	// FrameInvalidBufferUTF8 means an editing snapshot was not valid UTF-8.
	FrameInvalidBufferUTF8
	// FrameMissingProbeNonce means a probe nonce was absent.
	FrameMissingProbeNonce
	// FrameNonDecimalProbeNonce means a probe nonce was not ASCII decimal.
	FrameNonDecimalProbeNonce
	// FrameProbeNonceOutOfRange means a probe nonce exceeded uint64.
	FrameProbeNonceOutOfRange
	// FrameInvalidProbeResyncGrammar means a resync response lacked exactly two fields.
	FrameInvalidProbeResyncGrammar
	// FrameInvalidProbeResyncRequestID means a request ID was not canonical decimal.
	FrameInvalidProbeResyncRequestID
	// FrameProbeResyncRequestIDOutOfRange means a request ID was outside its shared bound.
	FrameProbeResyncRequestIDOutOfRange
	// FrameMissingFishPrintTerminator means a Fish snapshot lacked its final newline.
	FrameMissingFishPrintTerminator
	// FrameMissingExitStatus means command-stop had no status.
	FrameMissingExitStatus
	// FrameNonDecimalExitStatus means a status was not ASCII decimal.
	FrameNonDecimalExitStatus
	// FrameExitStatusOutOfRange means a status was outside 0 through 255.
	FrameExitStatusOutOfRange
	// FrameInvalidWorkingDirectory means a CWD was empty, relative, or too large.
	FrameInvalidWorkingDirectory
	// FrameInvalidReloadRequest means a reload nonce was invalid.
	FrameInvalidReloadRequest
	// FrameEmptySubmittedCommand means a lifecycle frame claimed an empty command.
	FrameEmptySubmittedCommand
	// FrameTooLarge means a frame exceeded its configured byte limit.
	FrameTooLarge
	// FrameTruncated means input ended before a frame terminator.
	FrameTruncated
)

// FrameError is a closed, content-free reason a shell event frame was rejected.
type FrameError struct {
	kind          FrameErrorKind
	observedBytes int
	limit         int
}

// Kind returns the rejection classification.
func (e *FrameError) Kind() FrameErrorKind { return e.kind }

// ObservedBytes returns the byte count for FrameTooLarge and FrameTruncated.
func (e *FrameError) ObservedBytes() int { return e.observedBytes }

// Limit returns the configured limit for FrameTooLarge.
func (e *FrameError) Limit() int { return e.limit }

// Error returns a content-free rejection description.
func (e *FrameError) Error() string {
	switch e.kind {
	case FrameEmpty:
		return "empty shell event frame"
	case FrameUnknownEvent:
		return "unknown shell event"
	case FrameMissingCursor:
		return "shell buffer cursor is missing"
	case FrameNonDecimalCursor:
		return "shell buffer cursor is not unsigned decimal"
	case FrameCursorOutOfRange:
		return "shell buffer cursor is out of range"
	case FrameCursorNotUTF8Boundary:
		return "shell buffer cursor splits a UTF-8 code point"
	case FrameInvalidBufferUTF8:
		return "shell buffer is not valid UTF-8"
	case FrameMissingProbeNonce:
		return "shell snapshot probe nonce is missing"
	case FrameNonDecimalProbeNonce:
		return "shell snapshot probe nonce is not unsigned decimal"
	case FrameProbeNonceOutOfRange:
		return "shell snapshot probe nonce is out of range"
	case FrameInvalidProbeResyncGrammar:
		return "shell probe resync response has invalid grammar"
	case FrameInvalidProbeResyncRequestID:
		return "shell probe resync request identifier is not canonical decimal"
	case FrameProbeResyncRequestIDOutOfRange:
		return "shell probe resync request identifier is out of range"
	case FrameMissingFishPrintTerminator:
		return "Fish shell snapshot print terminator is missing"
	case FrameMissingExitStatus:
		return "shell exit status is missing"
	case FrameNonDecimalExitStatus:
		return "shell exit status is not unsigned decimal"
	case FrameExitStatusOutOfRange:
		return "shell exit status is outside 0 through 255"
	case FrameInvalidWorkingDirectory:
		return "shell working directory is not a bounded absolute path"
	case FrameInvalidReloadRequest:
		return "active-session reload request is invalid"
	case FrameEmptySubmittedCommand:
		return "submitted command is empty"
	case FrameTooLarge:
		return fmt.Sprintf("shell event frame is %d bytes; limit is %d", e.observedBytes, e.limit)
	case FrameTruncated:
		return fmt.Sprintf("shell event frame ended after %d bytes without a terminator", e.observedBytes)
	default:
		return fmt.Sprintf("unknown shell event frame error (%d)", e.kind)
	}
}

// GoString returns a structural rejection representation.
func (e *FrameError) GoString() string {
	if e.kind == FrameTooLarge {
		return fmt.Sprintf("FrameTooLarge { observed_bytes: %d, limit: %d }", e.observedBytes, e.limit)
	}
	if e.kind == FrameTruncated {
		return fmt.Sprintf("TruncatedFrame { observed_bytes: %d }", e.observedBytes)
	}
	return fmt.Sprintf("FrameError(%d)", e.kind)
}

// RejectedFrame pairs a rejection with its stream position.
type RejectedFrame struct {
	position FramePosition
	err      *FrameError
}

// Position returns the rejected frame's stream position.
func (f RejectedFrame) Position() FramePosition { return f.position }

// Error returns why the frame was rejected.
func (f RejectedFrame) Error() *FrameError { return f.err }

// DecodedFrameKind identifies one complete result from the framed byte stream.
type DecodedFrameKind uint8

const (
	// DecodedEvent identifies a valid shell event.
	DecodedEvent DecodedFrameKind = iota + 1
	// DecodedRejected identifies an isolated protocol rejection.
	DecodedRejected
)

// DecodedFrame is one complete result from the framed byte stream.
type DecodedFrame struct {
	kind      DecodedFrameKind
	event     SequencedShellEvent
	rejection RejectedFrame
}

// Kind returns the decoded result variant.
func (f DecodedFrame) Kind() DecodedFrameKind { return f.kind }

// Position returns the frame's stream position.
func (f DecodedFrame) Position() FramePosition {
	if f.kind == DecodedEvent {
		return f.event.position
	}
	return f.rejection.position
}

// Event returns the sequenced event when Kind is DecodedEvent.
func (f DecodedFrame) Event() (SequencedShellEvent, bool) {
	return f.event, f.kind == DecodedEvent
}

// Rejection returns the rejected frame when Kind is DecodedRejected.
func (f DecodedFrame) Rejection() (RejectedFrame, bool) {
	return f.rejection, f.kind == DecodedRejected
}

// String returns a representation that redacts protocol payloads.
func (f DecodedFrame) String() string {
	if f.kind == DecodedEvent {
		return fmt.Sprintf("Event(%s)", f.event)
	}
	return fmt.Sprintf("Rejected(%#v)", f.rejection.err)
}

// GoString returns a representation that redacts protocol payloads.
func (f DecodedFrame) GoString() string { return f.String() }

// StreamResetErrorKind classifies a decoder or reducer epoch reset failure.
type StreamResetErrorKind uint8

const (
	// StreamResetEpochExhausted means no later numeric epoch exists.
	StreamResetEpochExhausted StreamResetErrorKind = iota + 1
	// StreamResetNonIncreasingEpoch means a reducer epoch did not increase.
	StreamResetNonIncreasingEpoch
)

// StreamResetError describes an invalid stream reset.
type StreamResetError struct{ kind StreamResetErrorKind }

// Kind returns the reset failure classification.
func (e *StreamResetError) Kind() StreamResetErrorKind { return e.kind }

// Error returns the reset failure description.
func (e *StreamResetError) Error() string {
	if e.kind == StreamResetEpochExhausted {
		return "shell event stream epoch is exhausted"
	}
	return "new shell event epoch must increase"
}

// InputGenerationError is returned when all local-input generations in an
// epoch have been issued.
type InputGenerationError struct{}

// Error returns the generation exhaustion description.
func (*InputGenerationError) Error() string { return "local shell-input generation is exhausted" }

// ProbeRequestErrorKind classifies why a synchronization probe could not begin.
type ProbeRequestErrorKind uint8

const (
	// ProbeCapabilityUnavailable means no probe handshake is active.
	ProbeCapabilityUnavailable ProbeRequestErrorKind = iota + 1
	// ProbeNotAtEditablePrompt means foreground or prompt state is unsafe.
	ProbeNotAtEditablePrompt
	// ProbeAlreadyPending means an ordinary probe remains outstanding.
	ProbeAlreadyPending
	// ProbeResyncRequired means counter recovery must complete first.
	ProbeResyncRequired
	// ProbeResyncPending means a recovery request remains outstanding.
	ProbeResyncPending
	// ProbeNonceExhausted means the adapter nonce space is exhausted.
	ProbeNonceExhausted
)

// ProbeRequestError describes why a synchronization probe could not begin.
type ProbeRequestError struct{ kind ProbeRequestErrorKind }

// Kind returns the probe failure classification.
func (e *ProbeRequestError) Kind() ProbeRequestErrorKind { return e.kind }

// Error returns the probe failure description.
func (e *ProbeRequestError) Error() string {
	switch e.kind {
	case ProbeCapabilityUnavailable:
		return "shell snapshot probe capability is unavailable"
	case ProbeNotAtEditablePrompt:
		return "shell is not at an editable prompt"
	case ProbeAlreadyPending:
		return "a shell snapshot probe is already pending"
	case ProbeResyncRequired:
		return "shell snapshot probe counter resync is required"
	case ProbeResyncPending:
		return "shell snapshot probe counter resync is pending"
	default:
		return "shell snapshot probe nonce is exhausted"
	}
}

// ProbeResyncRequestErrorKind classifies why adapter-counter recovery could not begin.
type ProbeResyncRequestErrorKind uint8

const (
	// ResyncCapabilityUnavailable means no probe handshake is active.
	ResyncCapabilityUnavailable ProbeResyncRequestErrorKind = iota + 1
	// ResyncNotAtEditablePrompt means foreground or prompt state is unsafe.
	ResyncNotAtEditablePrompt
	// ResyncNotRequired means no mismatch currently requires recovery.
	ResyncNotRequired
	// ResyncAlreadyPending means a recovery request remains outstanding.
	ResyncAlreadyPending
	// ResyncRequestIDExhausted means all shared request IDs were issued.
	ResyncRequestIDExhausted
)

// ProbeResyncRequestError describes why counter recovery could not begin.
type ProbeResyncRequestError struct{ kind ProbeResyncRequestErrorKind }

// Kind returns the resynchronization failure classification.
func (e *ProbeResyncRequestError) Kind() ProbeResyncRequestErrorKind { return e.kind }

// Error returns the resynchronization failure description.
func (e *ProbeResyncRequestError) Error() string {
	switch e.kind {
	case ResyncCapabilityUnavailable:
		return "shell snapshot probe capability is unavailable"
	case ResyncNotAtEditablePrompt:
		return "shell is not at an editable prompt"
	case ResyncNotRequired:
		return "shell snapshot probe counter resync is not required"
	case ResyncAlreadyPending:
		return "a shell snapshot probe counter resync is already pending"
	default:
		return "shell snapshot probe counter resync identifiers are exhausted"
	}
}

// Decoder incrementally decodes NUL-framed shell events with bounded retained storage.
type Decoder struct {
	pending       []byte
	oversized     bool
	oversizedSize int
	frameLimit    int
	epoch         StreamEpoch
	nextSequence  FrameSequence
}

// NewDecoder creates a decoder for an explicit stream epoch at the hard frame limit.
func NewDecoder(epoch StreamEpoch) *Decoder { return NewDecoderWithFrameLimit(epoch, MaxFrameBytes) }

// NewDecoderWithFrameLimit creates a decoder with an explicit limit capped by
// MaxFrameBytes. A negative limit accepts no nonempty frame bytes.
func NewDecoderWithFrameLimit(epoch StreamEpoch, frameLimit int) *Decoder {
	if frameLimit < 0 {
		frameLimit = 0
	}
	frameLimit = min(frameLimit, MaxFrameBytes)
	return &Decoder{
		pending:    make([]byte, 0, min(frameLimit, 4*1024)),
		frameLimit: frameLimit,
		epoch:      epoch,
	}
}

// Epoch returns the current stream epoch.
func (d *Decoder) Epoch() StreamEpoch { return d.epoch }

// FrameLimit returns the maximum retained frame bytes.
func (d *Decoder) FrameLimit() int { return d.frameLimit }

// PendingLen returns the retained bytes for an incomplete frame.
func (d *Decoder) PendingLen() int { return len(d.pending) }

// Discarding reports whether an oversized frame is being discarded through its NUL.
func (d *Decoder) Discarding() bool { return d.oversized }

// Push consumes a stream chunk and synchronously emits complete results in wire order.
func (d *Decoder) Push(chunk []byte, emit func(DecodedFrame)) {
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

// Finish rejects one unterminated frame at end of input and clears it. The
// boolean is false when no frame is pending.
func (d *Decoder) Finish() (DecodedFrame, bool) {
	if d.oversized {
		frame := d.rejected(&FrameError{
			kind: FrameTooLarge, observedBytes: d.oversizedSize, limit: d.frameLimit,
		})
		d.oversized = false
		d.oversizedSize = 0
		return frame, true
	}
	if len(d.pending) == 0 {
		return DecodedFrame{}, false
	}
	observed := len(d.pending)
	clear(d.pending)
	d.pending = d.pending[:0]
	return d.rejected(&FrameError{kind: FrameTruncated, observedBytes: observed}), true
}

// ResetStream discards partial input and advances to a fresh stream epoch.
func (d *Decoder) ResetStream() (StreamEpoch, error) {
	epoch, ok := d.epoch.Next()
	if !ok {
		return StreamEpoch{}, &StreamResetError{kind: StreamResetEpochExhausted}
	}
	clear(d.pending)
	d.pending = d.pending[:0]
	d.oversized = false
	d.oversizedSize = 0
	d.epoch = epoch
	d.nextSequence = FrameSequence{}
	return epoch, nil
}

// String returns a representation that redacts retained bytes.
func (d Decoder) String() string {
	oversized := "None"
	if d.oversized {
		oversized = fmt.Sprintf("Some(%d)", d.oversizedSize)
	}
	return fmt.Sprintf(
		"ShellEventDecoder { pending_bytes: %d, oversized_bytes: %s, frame_limit: %d, epoch: %d, next_sequence: (%d,%d) }",
		len(d.pending), oversized, d.frameLimit, d.epoch.value, d.nextSequence.high, d.nextSequence.low,
	)
}

// GoString returns a representation that redacts retained bytes.
func (d Decoder) GoString() string { return d.String() }

func (d *Decoder) finishTerminatedFrame() DecodedFrame {
	if d.oversized {
		frame := d.rejected(&FrameError{
			kind: FrameTooLarge, observedBytes: d.oversizedSize, limit: d.frameLimit,
		})
		d.oversized = false
		d.oversizedSize = 0
		return frame
	}
	event, err := parseFrame(d.pending)
	clear(d.pending)
	d.pending = d.pending[:0]
	if err != nil {
		return d.rejected(err)
	}
	position := d.takePosition()
	return DecodedFrame{
		kind:  DecodedEvent,
		event: SequencedShellEvent{position: position, event: event},
	}
}

func (d *Decoder) rejected(err *FrameError) DecodedFrame {
	return DecodedFrame{
		kind:      DecodedRejected,
		rejection: RejectedFrame{position: d.takePosition(), err: err},
	}
}

func (d *Decoder) takePosition() FramePosition {
	position := FramePosition{epoch: d.epoch, sequence: d.nextSequence}
	d.nextSequence = d.nextSequence.nextSaturating()
	return position
}

type cursorUnit uint8

const (
	cursorBytes cursorUnit = iota
	cursorCharacters
	cursorFishCharacters
)

func parseFrame(frame []byte) (ShellEvent, *FrameError) {
	if len(frame) == 0 {
		return ShellEvent{}, frameError(FrameEmpty)
	}
	if payload, ok := bytes.CutPrefix(frame, probeResyncPrefix); ok {
		response, err := parseProbeResync(payload)
		return ShellEvent{kind: EventProbeResync, probeResync: response}, err
	}
	if payload, ok := bytes.CutPrefix(frame, probeBufferBytePrefix); ok {
		snapshot, err := parseProbeBuffer(payload, cursorBytes)
		return ShellEvent{kind: EventBuffer, buffer: snapshot}, err
	}
	if payload, ok := bytes.CutPrefix(frame, probeBufferCharacterPrefix); ok {
		snapshot, err := parseProbeBuffer(payload, cursorCharacters)
		return ShellEvent{kind: EventBuffer, buffer: snapshot}, err
	}
	if payload, ok := bytes.CutPrefix(frame, probeBufferFishPrefix); ok {
		snapshot, err := parseProbeBuffer(payload, cursorFishCharacters)
		return ShellEvent{kind: EventBuffer, buffer: snapshot}, err
	}
	if payload, ok := bytes.CutPrefix(frame, bufferBytePrefix); ok {
		snapshot, err := parseBuffer(payload, cursorBytes, SnapshotNonce{}, false)
		return ShellEvent{kind: EventBuffer, buffer: snapshot}, err
	}
	if payload, ok := bytes.CutPrefix(frame, bufferCharacterPrefix); ok {
		snapshot, err := parseBuffer(payload, cursorCharacters, SnapshotNonce{}, false)
		return ShellEvent{kind: EventBuffer, buffer: snapshot}, err
	}
	if bytes.Equal(frame, promptReady) {
		return ShellEvent{kind: EventPromptReady}, nil
	}
	if path, ok := bytes.CutPrefix(frame, workingDirectoryPrefix); ok {
		if len(path) == 0 || len(path) > maxWorkingDirectoryBytes || path[0] != '/' {
			return ShellEvent{}, frameError(FrameInvalidWorkingDirectory)
		}
		return ShellEvent{
			kind:      EventWorkingDirectory,
			directory: ShellWorkingDirectory{bytes: bytes.Clone(path)},
		}, nil
	}
	if nonce, ok := bytes.CutPrefix(frame, []byte(reloadRequestPrefix)); ok {
		if !validCanonicalDecimal(nonce) || len(nonce) > 10 {
			return ShellEvent{}, frameError(FrameInvalidReloadRequest)
		}
		value, err := strconv.ParseUint(string(nonce), 10, 32)
		if err != nil || value == 0 {
			return ShellEvent{}, frameError(FrameInvalidReloadRequest)
		}
		return ShellEvent{kind: EventReloadRequest, reloadRequest: ReloadRequest{nonce: uint32(value)}}, nil
	}
	if bytes.Equal(frame, commandStartUnknown) {
		return ShellEvent{kind: EventCommandStartUnknown}, nil
	}
	if command, ok := bytes.CutPrefix(frame, commandStartPrefix); ok {
		if len(command) == 0 {
			return ShellEvent{}, frameError(FrameEmptySubmittedCommand)
		}
		return ShellEvent{
			kind:    EventCommandStart,
			command: SubmittedCommand{bytes: bytes.Clone(command)},
		}, nil
	}
	if bytes.Equal(frame, capabilityNative) {
		return ShellEvent{
			kind:       EventCapability,
			capability: CapabilityAnnouncement{capability: BufferSyncNative},
		}, nil
	}
	if nonce, ok := bytes.CutPrefix(frame, capabilityProbePrefix); ok {
		value, err := parseProbeNonce(nonce)
		if err != nil {
			return ShellEvent{}, err
		}
		return ShellEvent{
			kind: EventCapability,
			capability: CapabilityAnnouncement{
				capability: BufferSyncProbe, lastProbeNonce: value, hasLastNonce: true,
			},
		}, nil
	}
	if bytes.Equal(frame, capabilityUnavailable) {
		return ShellEvent{
			kind:       EventCapability,
			capability: CapabilityAnnouncement{capability: BufferSyncUnavailable},
		}, nil
	}
	status, ok := bytes.CutPrefix(frame, commandStopPrefix)
	if !ok {
		return ShellEvent{}, frameError(FrameUnknownEvent)
	}
	value, err := parseExitStatus(status)
	if err != nil {
		return ShellEvent{}, err
	}
	return ShellEvent{kind: EventCommandStop, status: value}, nil
}

func parseProbeResync(frame []byte) (ProbeResyncResponse, *FrameError) {
	fields := bytes.Split(frame, []byte{':'})
	if len(fields) != 2 {
		return ProbeResyncResponse{}, frameError(FrameInvalidProbeResyncGrammar)
	}
	request, ok := parseCanonicalDecimal(fields[0])
	if !ok {
		return ProbeResyncResponse{}, frameError(FrameInvalidProbeResyncRequestID)
	}
	requestID, err := shellcontrol.NewProbeResyncRequestID(request)
	if err != nil {
		return ProbeResyncResponse{}, frameError(FrameProbeResyncRequestIDOutOfRange)
	}
	lastNonce, nonceErr := parseProbeNonce(fields[1])
	if nonceErr != nil {
		return ProbeResyncResponse{}, nonceErr
	}
	return ProbeResyncResponse{requestID: requestID, lastProbeNonce: lastNonce}, nil
}

func parseProbeBuffer(frame []byte, unit cursorUnit) (BufferSnapshot, *FrameError) {
	separator := bytes.IndexByte(frame, ':')
	if separator < 0 {
		return BufferSnapshot{}, frameError(FrameMissingProbeNonce)
	}
	nonce, err := parseProbeNonce(frame[:separator])
	if err != nil {
		return BufferSnapshot{}, err
	}
	return parseBuffer(frame[separator+1:], unit, nonce, true)
}

func parseBuffer(
	frame []byte,
	unit cursorUnit,
	probeNonce SnapshotNonce,
	hasProbe bool,
) (BufferSnapshot, *FrameError) {
	separator := bytes.IndexByte(frame, ':')
	if separator < 0 {
		return BufferSnapshot{}, frameError(FrameMissingCursor)
	}
	cursor, err := parseCursor(frame[:separator])
	if err != nil {
		return BufferSnapshot{}, err
	}
	payload := frame[separator+1:]
	if unit == cursorFishCharacters {
		if len(payload) == 0 || payload[len(payload)-1] != '\n' {
			return BufferSnapshot{}, frameError(FrameMissingFishPrintTerminator)
		}
		payload = payload[:len(payload)-1]
	}
	if !utf8.Valid(payload) {
		return BufferSnapshot{}, frameError(FrameInvalidBufferUTF8)
	}
	byteCursor := cursor
	switch unit {
	case cursorBytes:
		if cursor > len(payload) {
			return BufferSnapshot{}, frameError(FrameCursorOutOfRange)
		}
		if !utf8.Valid(payload[:cursor]) {
			return BufferSnapshot{}, frameError(FrameCursorNotUTF8Boundary)
		}
	default:
		byteCursor = characterCursorToByte(payload, cursor)
		if byteCursor < 0 {
			return BufferSnapshot{}, frameError(FrameCursorOutOfRange)
		}
	}
	return BufferSnapshot{
		bytes: bytes.Clone(payload), cursor: byteCursor, probeNonce: probeNonce, hasProbe: hasProbe,
	}, nil
}

func characterCursorToByte(payload []byte, cursor int) int {
	characters := 0
	for index := range string(payload) {
		if characters == cursor {
			return index
		}
		characters++
	}
	if characters == cursor {
		return len(payload)
	}
	return -1
}

func parseCanonicalDecimal(value []byte) (uint64, bool) {
	if !validCanonicalDecimal(value) {
		return 0, false
	}
	parsed, err := strconv.ParseUint(string(value), 10, 64)
	return parsed, err == nil
}

func validCanonicalDecimal(value []byte) bool {
	if len(value) == 0 || len(value) > 1 && value[0] == '0' {
		return false
	}
	return decimalBytes(value)
}

func parseProbeNonce(value []byte) (SnapshotNonce, *FrameError) {
	if len(value) == 0 {
		return SnapshotNonce{}, frameError(FrameMissingProbeNonce)
	}
	if !decimalBytes(value) {
		return SnapshotNonce{}, frameError(FrameNonDecimalProbeNonce)
	}
	parsed, err := strconv.ParseUint(string(value), 10, 64)
	if err != nil {
		return SnapshotNonce{}, frameError(FrameProbeNonceOutOfRange)
	}
	return SnapshotNonce{value: parsed}, nil
}

func parseCursor(value []byte) (int, *FrameError) {
	if len(value) == 0 {
		return 0, frameError(FrameMissingCursor)
	}
	if !decimalBytes(value) {
		return 0, frameError(FrameNonDecimalCursor)
	}
	cursor := 0
	for _, digit := range value {
		value := int(digit - '0')
		if cursor > (math.MaxInt-value)/10 {
			return 0, frameError(FrameCursorOutOfRange)
		}
		cursor = cursor*10 + value
	}
	return cursor, nil
}

func parseExitStatus(value []byte) (ShellExitStatus, *FrameError) {
	if len(value) == 0 {
		return ShellExitStatus{}, frameError(FrameMissingExitStatus)
	}
	if !decimalBytes(value) {
		return ShellExitStatus{}, frameError(FrameNonDecimalExitStatus)
	}
	status := uint16(0)
	for _, digit := range value {
		if status > 255/10 {
			return ShellExitStatus{}, frameError(FrameExitStatusOutOfRange)
		}
		status = status*10 + uint16(digit-'0')
		if status > math.MaxUint8 {
			return ShellExitStatus{}, frameError(FrameExitStatusOutOfRange)
		}
	}
	return ShellExitStatus{value: uint8(status)}, nil
}

func decimalBytes(value []byte) bool {
	for _, char := range value {
		if char < '0' || char > '9' {
			return false
		}
	}
	return true
}

func frameError(kind FrameErrorKind) *FrameError { return &FrameError{kind: kind} }

func saturatedIncrement(value int) int {
	if value == math.MaxInt {
		return value
	}
	return value + 1
}
