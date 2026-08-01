package shellevents

import (
	"bytes"
	"fmt"
	"math"

	"github.com/rselbach/argmax/internal/shellcontrol"
)

// SynchronizationState reports whether reducer state can be trusted for
// suggestions or attribution.
type SynchronizationState uint8

const (
	// StateSynchronized follows an authoritative snapshot or prompt boundary.
	StateSynchronized SynchronizationState = iota + 1
	// StateDesynchronized follows a rejected or impossible frame.
	StateDesynchronized
)

// ForegroundCommandState reports whether a foreground command owns the terminal.
type ForegroundCommandState uint8

const (
	// ForegroundIdle means no foreground command is active.
	ForegroundIdle ForegroundCommandState = iota + 1
	// ForegroundRunning means a foreground command is active.
	ForegroundRunning
	// ForegroundUnknown means protocol state cannot determine ownership safely.
	ForegroundUnknown
)

// AttributionSource identifies the source of exact completed-command bytes.
type AttributionSource uint8

const (
	// AttributionLifecycleFrame means exact bytes came from a preexec lifecycle frame.
	AttributionLifecycleFrame AttributionSource = iota + 1
)

// CompletedCommand is safe to pass to history and learning.
type CompletedCommand struct {
	command SubmittedCommand
	status  ShellExitStatus
	source  AttributionSource
}

// Command returns the exact submitted command.
func (c CompletedCommand) Command() SubmittedCommand { return c.command }

// Status returns the reported command status.
func (c CompletedCommand) Status() ShellExitStatus { return c.status }

// Source returns how exact command bytes were obtained.
func (c CompletedCommand) Source() AttributionSource { return c.source }

// String returns a representation that redacts command content.
func (c CompletedCommand) String() string {
	return fmt.Sprintf(
		"CompletedCommand { command: %s, status: %d, source: %d }",
		c.command, c.status.value, c.source,
	)
}

// GoString returns a representation that redacts command content.
func (c CompletedCommand) GoString() string { return c.String() }

// LifecycleError identifies an impossible command-lifecycle transition.
type LifecycleError uint8

const (
	// LifecycleDuplicateCommandStart means a second start arrived while running.
	LifecycleDuplicateCommandStart LifecycleError = iota + 1
	// LifecycleCommandStopWithoutStart means a stop arrived while idle.
	LifecycleCommandStopWithoutStart
	// LifecycleBufferWhileCommandRunning means a snapshot arrived while running.
	LifecycleBufferWhileCommandRunning
	// LifecyclePromptWhileCommandRunning means a prompt arrived before command stop.
	LifecyclePromptWhileCommandRunning
)

// SnapshotRejectionKind classifies a syntactically valid snapshot that was not authoritative.
type SnapshotRejectionKind uint8

const (
	// SnapshotMissingProbeNonce means a native snapshot came from a probe-only adapter.
	SnapshotMissingProbeNonce SnapshotRejectionKind = iota + 1
	// SnapshotUnexpectedProbeNonce means a numbered response came from a native adapter.
	SnapshotUnexpectedProbeNonce
	// SnapshotUncorrelatedNative means a native callback could not identify observed input.
	SnapshotUncorrelatedNative
	// SnapshotProbeNonceMismatch means a response did not match the pending probe.
	SnapshotProbeNonceMismatch
	// SnapshotStaleProbeGeneration means matching input has since changed.
	SnapshotStaleProbeGeneration
)

// SnapshotRejection explains why a valid buffer snapshot was not authoritative.
type SnapshotRejection struct {
	kind        SnapshotRejectionKind
	nonce       SnapshotNonce
	expected    SnapshotNonce
	hasExpected bool
	requested   InputGeneration
	current     InputGeneration
}

// Kind returns the rejection variant.
func (r SnapshotRejection) Kind() SnapshotRejectionKind { return r.kind }

// Nonce returns the unexpected or received nonce for nonce-bearing variants.
func (r SnapshotRejection) Nonce() SnapshotNonce { return r.nonce }

// ExpectedNonce returns the expected nonce and whether a probe was pending.
func (r SnapshotRejection) ExpectedNonce() (SnapshotNonce, bool) {
	return r.expected, r.hasExpected
}

// RequestedGeneration returns the generation that requested a stale response.
func (r SnapshotRejection) RequestedGeneration() InputGeneration { return r.requested }

// CurrentGeneration returns current generation for stale and uncorrelated responses.
func (r SnapshotRejection) CurrentGeneration() InputGeneration { return r.current }

// ProbeResyncRejection explains why a valid adapter-counter response was not authoritative.
type ProbeResyncRejection struct {
	expected    shellcontrol.ProbeResyncRequestID
	hasExpected bool
	received    shellcontrol.ProbeResyncRequestID
}

// ExpectedRequestID returns the pending ID and whether one was pending.
func (r ProbeResyncRejection) ExpectedRequestID() (shellcontrol.ProbeResyncRequestID, bool) {
	return r.expected, r.hasExpected
}

// ReceivedRequestID returns the identifier carried by the response.
func (r ProbeResyncRejection) ReceivedRequestID() shellcontrol.ProbeResyncRequestID {
	return r.received
}

// CommandStartUpdate describes a successfully observed command start.
type CommandStartUpdate struct {
	source                 AttributionSource
	hasSource              bool
	preexecMatchesSnapshot bool
	hasPreexecMatch        bool
}

// Source returns the attribution selected for a matching stop, if available.
func (u CommandStartUpdate) Source() (AttributionSource, bool) { return u.source, u.hasSource }

// PreexecMatchesSnapshot reports whether preexec bytes matched a preceding
// authoritative snapshot, and whether both values existed for comparison.
func (u CommandStartUpdate) PreexecMatchesSnapshot() (bool, bool) {
	return u.preexecMatchesSnapshot, u.hasPreexecMatch
}

// StreamOrderRejection describes a duplicate, stale, skipped, or wrong-epoch frame.
type StreamOrderRejection struct {
	received     FramePosition
	lastAccepted FramePosition
	hasLast      bool
}

// Received returns the rejected position.
func (r StreamOrderRejection) Received() FramePosition { return r.received }

// LastAccepted returns the last safely applied position and whether one exists.
func (r StreamOrderRejection) LastAccepted() (FramePosition, bool) {
	return r.lastAccepted, r.hasLast
}

// StateUpdateKind identifies the result of consuming one decoded frame.
type StateUpdateKind uint8

const (
	// UpdateBufferSynchronized means an authoritative snapshot synchronized state.
	UpdateBufferSynchronized StateUpdateKind = iota + 1
	// UpdatePromptReady means an authoritative prompt boundary synchronized state.
	UpdatePromptReady
	// UpdateCommandStarted means a foreground command began.
	UpdateCommandStarted
	// UpdateCommandStopped means a command stopped with exact attribution.
	UpdateCommandStopped
	// UpdateCommandStoppedWithoutAttribution means exact submitted bytes were unavailable.
	UpdateCommandStoppedWithoutAttribution
	// UpdateCapabilityChanged means adapter capability changed.
	UpdateCapabilityChanged
	// UpdateProbeResynchronized means a matching response established a counter baseline.
	UpdateProbeResynchronized
	// UpdateProbeResyncRejected means a wrong response left recovery state unchanged.
	UpdateProbeResyncRejected
	// UpdateWorkingDirectoryChanged means the shell reported an authoritative CWD.
	UpdateWorkingDirectoryChanged
	// UpdateReloadRequested means a child requested a correlated configuration reload.
	UpdateReloadRequested
	// UpdateFrameRejected means a protocol rejection desynchronized state.
	UpdateFrameRejected
	// UpdateLifecycleRejected means an impossible lifecycle transition desynchronized state.
	UpdateLifecycleRejected
	// UpdateLifecycleSuppressed means a valid lifecycle event arrived while desynchronized.
	UpdateLifecycleSuppressed
	// UpdateSnapshotRejected means a snapshot failed authority correlation.
	UpdateSnapshotRejected
	// UpdateStreamOrderRejected means frame order or epoch was invalid.
	UpdateStreamOrderRejected
)

// StateUpdate is the result of consuming one DecodedFrame.
type StateUpdate struct {
	kind              StateUpdateKind
	recovered         bool
	commandStart      CommandStartUpdate
	completed         CompletedCommand
	status            ShellExitStatus
	capability        BufferSyncCapability
	probeResync       ProbeResyncResponse
	probeRejection    ProbeResyncRejection
	directory         ShellWorkingDirectory
	reloadRequest     ReloadRequest
	frameErr          *FrameError
	lifecycleErr      LifecycleError
	snapshotRejection SnapshotRejection
	orderRejection    StreamOrderRejection
}

// Kind returns the update variant.
func (u StateUpdate) Kind() StateUpdateKind { return u.kind }

// Recovered reports whether a buffer or prompt update recovered desynchronized state.
func (u StateUpdate) Recovered() (bool, bool) {
	return u.recovered, u.kind == UpdateBufferSynchronized || u.kind == UpdatePromptReady
}

// CommandStart returns command-start details when Kind is UpdateCommandStarted.
func (u StateUpdate) CommandStart() (CommandStartUpdate, bool) {
	return u.commandStart, u.kind == UpdateCommandStarted
}

// CompletedCommand returns completion details when Kind is UpdateCommandStopped.
func (u StateUpdate) CompletedCommand() (CompletedCommand, bool) {
	return u.completed, u.kind == UpdateCommandStopped
}

// Status returns the status when Kind is UpdateCommandStoppedWithoutAttribution.
func (u StateUpdate) Status() (ShellExitStatus, bool) {
	return u.status, u.kind == UpdateCommandStoppedWithoutAttribution
}

// Capability returns the capability when Kind is UpdateCapabilityChanged.
func (u StateUpdate) Capability() (BufferSyncCapability, bool) {
	return u.capability, u.kind == UpdateCapabilityChanged
}

// ProbeResync returns the response when Kind is UpdateProbeResynchronized.
func (u StateUpdate) ProbeResync() (ProbeResyncResponse, bool) {
	return u.probeResync, u.kind == UpdateProbeResynchronized
}

// ProbeResyncRejection returns details when Kind is UpdateProbeResyncRejected.
func (u StateUpdate) ProbeResyncRejection() (ProbeResyncRejection, bool) {
	return u.probeRejection, u.kind == UpdateProbeResyncRejected
}

// WorkingDirectory returns the directory when Kind is UpdateWorkingDirectoryChanged.
func (u StateUpdate) WorkingDirectory() (ShellWorkingDirectory, bool) {
	return u.directory, u.kind == UpdateWorkingDirectoryChanged
}

// ReloadRequest returns the request when Kind is UpdateReloadRequested.
func (u StateUpdate) ReloadRequest() (ReloadRequest, bool) {
	return u.reloadRequest, u.kind == UpdateReloadRequested
}

// FrameError returns the rejection when Kind is UpdateFrameRejected.
func (u StateUpdate) FrameError() (*FrameError, bool) {
	return u.frameErr, u.kind == UpdateFrameRejected
}

// LifecycleError returns the transition failure when Kind is UpdateLifecycleRejected.
func (u StateUpdate) LifecycleError() (LifecycleError, bool) {
	return u.lifecycleErr, u.kind == UpdateLifecycleRejected
}

// SnapshotRejection returns details when Kind is UpdateSnapshotRejected.
func (u StateUpdate) SnapshotRejection() (SnapshotRejection, bool) {
	return u.snapshotRejection, u.kind == UpdateSnapshotRejected
}

// StreamOrderRejection returns details when Kind is UpdateStreamOrderRejected.
func (u StateUpdate) StreamOrderRejection() (StreamOrderRejection, bool) {
	return u.orderRejection, u.kind == UpdateStreamOrderRejected
}

// String returns a representation that redacts protocol payloads.
func (u StateUpdate) String() string {
	switch u.kind {
	case UpdateCommandStopped:
		return fmt.Sprintf("CommandStopped(%s)", u.completed)
	case UpdateWorkingDirectoryChanged:
		return fmt.Sprintf("WorkingDirectoryChanged(%s)", u.directory)
	default:
		return fmt.Sprintf("StateUpdate(%d)", u.kind)
	}
}

// GoString returns a representation that redacts protocol payloads.
func (u StateUpdate) GoString() string { return u.String() }

type pendingProbe struct {
	nonce      SnapshotNonce
	generation InputGeneration
}

type attribution struct {
	command SubmittedCommand
	source  AttributionSource
}

// ShellSessionState is the ordered reducer that owns synchronization and exact
// command attribution.
type ShellSessionState struct {
	epoch                     StreamEpoch
	lastPosition              FramePosition
	hasLastPosition           bool
	orderFaulted              bool
	synchronization           SynchronizationState
	foreground                ForegroundCommandState
	promptReady               bool
	capability                BufferSyncCapability
	confirmedProbeNonce       SnapshotNonce
	hasConfirmedProbeNonce    bool
	pendingProbe              pendingProbe
	hasPendingProbe           bool
	probeResyncRequired       bool
	pendingProbeResync        shellcontrol.ProbeResyncRequestID
	hasPendingProbeResync     bool
	lastProbeResyncRequestID  shellcontrol.ProbeResyncRequestID
	hasLastProbeResyncID      bool
	buffer                    BufferSnapshot
	hasBuffer                 bool
	inputGeneration           InputGeneration
	inputUnacknowledged       bool
	bufferGeneration          InputGeneration
	hasBufferGeneration       bool
	bufferObservedSincePrompt bool
	attribution               attribution
	hasAttribution            bool
	lastStatus                ShellExitStatus
	hasLastStatus             bool
}

// NewShellSessionState creates desynchronized state for an explicit decoder epoch.
func NewShellSessionState(epoch StreamEpoch) *ShellSessionState {
	return &ShellSessionState{
		epoch:           epoch,
		synchronization: StateDesynchronized,
		foreground:      ForegroundUnknown,
		capability:      BufferSyncUnknown,
		inputGeneration: InputGeneration{epoch: epoch},
	}
}

// Epoch returns the active stream epoch.
func (s *ShellSessionState) Epoch() StreamEpoch { return s.epoch }

// Buffer returns the latest authoritative editing snapshot and whether one exists.
func (s *ShellSessionState) Buffer() (BufferSnapshot, bool) { return s.buffer, s.hasBuffer }

// InputGeneration returns current local-input authority generation.
func (s *ShellSessionState) InputGeneration() InputGeneration { return s.inputGeneration }

// BufferGeneration returns the accepted snapshot generation and whether one exists.
func (s *ShellSessionState) BufferGeneration() (InputGeneration, bool) {
	return s.bufferGeneration, s.hasBufferGeneration
}

// Synchronization returns whether session state is authoritative.
func (s *ShellSessionState) Synchronization() SynchronizationState { return s.synchronization }

// Foreground returns foreground command state.
func (s *ShellSessionState) Foreground() ForegroundCommandState { return s.foreground }

// Capability returns runtime adapter capability.
func (s *ShellSessionState) Capability() BufferSyncCapability { return s.capability }

// PendingProbeNonce returns the one outstanding ordinary probe nonce, if any.
func (s *ShellSessionState) PendingProbeNonce() (SnapshotNonce, bool) {
	return s.pendingProbe.nonce, s.hasPendingProbe
}

// ConfirmedProbeNonce returns the last ordinary nonce confirmed by the adapter.
func (s *ShellSessionState) ConfirmedProbeNonce() (SnapshotNonce, bool) {
	return s.confirmedProbeNonce, s.hasConfirmedProbeNonce
}

// ProbeResyncRequired reports whether explicit adapter-counter recovery is required.
func (s *ShellSessionState) ProbeResyncRequired() bool { return s.probeResyncRequired }

// PendingProbeResyncRequestID returns the outstanding recovery request, if any.
func (s *ShellSessionState) PendingProbeResyncRequestID() (shellcontrol.ProbeResyncRequestID, bool) {
	return s.pendingProbeResync, s.hasPendingProbeResync
}

// LastPosition returns the most recently safely applied position, if any.
func (s *ShellSessionState) LastPosition() (FramePosition, bool) {
	return s.lastPosition, s.hasLastPosition
}

// LastStatus returns the most recently completed command status, if any.
func (s *ShellSessionState) LastStatus() (ShellExitStatus, bool) {
	return s.lastStatus, s.hasLastStatus
}

// SuggestionsAllowed reports whether suggestions may be computed and rendered safely.
func (s *ShellSessionState) SuggestionsAllowed() bool {
	return s.synchronization == StateSynchronized &&
		s.foreground == ForegroundIdle &&
		s.promptReady &&
		s.capability.SupportsLiveSnapshots() &&
		!s.probeResyncRequired &&
		!s.hasPendingProbeResync
}

// ObserveLocalInput invalidates the current snapshot before locally forwarded
// editing input and returns the new generation.
func (s *ShellSessionState) ObserveLocalInput() (InputGeneration, error) {
	if s.inputGeneration.sequence == math.MaxUint64 {
		s.invalidateAuthority()
		return InputGeneration{}, &InputGenerationError{}
	}
	s.inputGeneration = InputGeneration{
		epoch: s.epoch, sequence: s.inputGeneration.sequence + 1,
	}
	s.inputUnacknowledged = true
	s.synchronization = StateDesynchronized
	s.clearBuffer()
	s.bufferObservedSincePrompt = false
	s.hasAttribution = false
	return s.inputGeneration, nil
}

// ProbeAvailable reports whether BeginSyncProbe would issue a probe now.
func (s *ShellSessionState) ProbeAvailable() bool {
	return s.capability == BufferSyncProbe &&
		s.foreground == ForegroundIdle &&
		s.promptReady &&
		!s.hasPendingProbe &&
		!s.probeResyncRequired &&
		!s.hasPendingProbeResync
}

// BeginSyncProbe reserves the next nonce before the caller injects SyncProbeSequence.
func (s *ShellSessionState) BeginSyncProbe() (SnapshotNonce, error) {
	switch {
	case s.capability != BufferSyncProbe:
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeCapabilityUnavailable}
	case s.foreground != ForegroundIdle || !s.promptReady:
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeNotAtEditablePrompt}
	case s.hasPendingProbeResync:
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeResyncPending}
	case s.probeResyncRequired:
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeResyncRequired}
	case s.hasPendingProbe:
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeAlreadyPending}
	case !s.hasConfirmedProbeNonce:
		s.invalidateEditingAuthority()
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeCapabilityUnavailable}
	case s.confirmedProbeNonce.value == math.MaxUint64:
		s.invalidateEditingAuthority()
		return SnapshotNonce{}, &ProbeRequestError{kind: ProbeNonceExhausted}
	}
	nonce := SnapshotNonce{value: s.confirmedProbeNonce.value + 1}
	s.pendingProbe = pendingProbe{nonce: nonce, generation: s.inputGeneration}
	s.hasPendingProbe = true
	return nonce, nil
}

// BeginProbeResync reserves a distinct request for explicit adapter-counter recovery.
func (s *ShellSessionState) BeginProbeResync() (shellcontrol.ProbeResyncRequestID, error) {
	switch {
	case s.capability != BufferSyncProbe:
		return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{
			kind: ResyncCapabilityUnavailable,
		}
	case s.foreground != ForegroundIdle || !s.promptReady:
		return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{
			kind: ResyncNotAtEditablePrompt,
		}
	case s.hasPendingProbeResync:
		return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{
			kind: ResyncAlreadyPending,
		}
	case !s.probeResyncRequired:
		return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{kind: ResyncNotRequired}
	}
	value := uint64(1)
	if s.hasLastProbeResyncID {
		if s.lastProbeResyncRequestID.Value() == shellcontrol.MaxProbeResyncRequestID {
			return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{
				kind: ResyncRequestIDExhausted,
			}
		}
		value = s.lastProbeResyncRequestID.Value() + 1
	}
	request, err := shellcontrol.NewProbeResyncRequestID(value)
	if err != nil {
		return shellcontrol.ProbeResyncRequestID{}, &ProbeResyncRequestError{
			kind: ResyncRequestIDExhausted,
		}
	}
	s.lastProbeResyncRequestID = request
	s.hasLastProbeResyncID = true
	s.pendingProbeResync = request
	s.hasPendingProbeResync = true
	return request, nil
}

// ResetStream resets state for a newer decoder epoch and requires new authority.
func (s *ShellSessionState) ResetStream(epoch StreamEpoch) error {
	if epoch.value <= s.epoch.value {
		return &StreamResetError{kind: StreamResetNonIncreasingEpoch}
	}
	s.epoch = epoch
	s.hasLastPosition = false
	s.orderFaulted = false
	s.capability = BufferSyncUnknown
	s.hasConfirmedProbeNonce = false
	s.hasPendingProbe = false
	s.probeResyncRequired = false
	s.hasPendingProbeResync = false
	s.inputGeneration = InputGeneration{epoch: epoch}
	s.inputUnacknowledged = false
	s.hasBufferGeneration = false
	s.invalidateAuthority()
	return nil
}

// Apply consumes every valid or rejected decoded frame in stream order.
func (s *ShellSessionState) Apply(frame DecodedFrame) StateUpdate {
	received := frame.Position()
	expected := FrameSequence{}
	hasExpected := true
	if s.hasLastPosition {
		expected, hasExpected = s.lastPosition.sequence.checkedNext()
	}
	if s.orderFaulted || received.epoch != s.epoch || !hasExpected || received.sequence != expected {
		rejection := StreamOrderRejection{
			received: received, lastAccepted: s.lastPosition, hasLast: s.hasLastPosition,
		}
		s.orderFaulted = true
		s.invalidateAuthority()
		return StateUpdate{kind: UpdateStreamOrderRejected, orderRejection: rejection}
	}
	s.lastPosition = received
	s.hasLastPosition = true

	switch frame.kind {
	case DecodedRejected:
		s.invalidateAuthority()
		return StateUpdate{kind: UpdateFrameRejected, frameErr: frame.rejection.err}
	case DecodedEvent:
		return s.applyEvent(frame.event.event)
	default:
		s.invalidateAuthority()
		return StateUpdate{kind: UpdateFrameRejected, frameErr: frameError(FrameUnknownEvent)}
	}
}

func (s *ShellSessionState) applyEvent(event ShellEvent) StateUpdate {
	switch event.kind {
	case EventBuffer:
		return s.synchronizeBuffer(event.buffer)
	case EventPromptReady:
		return s.synchronizePrompt()
	case EventCapability:
		return s.announceCapability(event.capability)
	case EventProbeResync:
		return s.applyProbeResync(event.probeResync)
	case EventCommandStart:
		return s.startCommand(event.command, true)
	case EventCommandStartUnknown:
		return s.startCommand(SubmittedCommand{}, false)
	case EventCommandStop:
		return s.stopCommand(event.status)
	case EventWorkingDirectory:
		return StateUpdate{kind: UpdateWorkingDirectoryChanged, directory: event.directory}
	case EventReloadRequest:
		return StateUpdate{kind: UpdateReloadRequested, reloadRequest: event.reloadRequest}
	default:
		s.invalidateAuthority()
		return StateUpdate{kind: UpdateFrameRejected, frameErr: frameError(FrameUnknownEvent)}
	}
}

func (s *ShellSessionState) synchronizeBuffer(buffer BufferSnapshot) StateUpdate {
	if s.foreground == ForegroundRunning {
		s.invalidateAuthority()
		return StateUpdate{
			kind: UpdateLifecycleRejected, lifecycleErr: LifecycleBufferWhileCommandRunning,
		}
	}
	if rejection, ok := s.correlateSnapshot(buffer); !ok {
		s.invalidateEditingAuthority()
		return StateUpdate{kind: UpdateSnapshotRejected, snapshotRejection: rejection}
	}
	if s.foreground != ForegroundIdle || !s.promptReady || !s.capability.SupportsLiveSnapshots() {
		s.invalidateEditingAuthority()
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}
	recovered := s.synchronization == StateDesynchronized
	s.synchronization = StateSynchronized
	s.buffer = buffer
	s.hasBuffer = true
	s.bufferGeneration = s.inputGeneration
	s.hasBufferGeneration = true
	s.inputUnacknowledged = false
	s.bufferObservedSincePrompt = true
	s.hasAttribution = false
	return StateUpdate{kind: UpdateBufferSynchronized, recovered: recovered}
}

func (s *ShellSessionState) synchronizePrompt() StateUpdate {
	if s.foreground == ForegroundRunning {
		s.invalidateAuthority()
		return StateUpdate{
			kind: UpdateLifecycleRejected, lifecycleErr: LifecyclePromptWhileCommandRunning,
		}
	}
	if s.capability == BufferSyncUnknown {
		s.invalidateAuthority()
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}
	if s.inputUnacknowledged {
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}
	if s.probeResyncRequired || s.hasPendingProbeResync {
		s.synchronization = StateDesynchronized
		s.foreground = ForegroundIdle
		s.promptReady = true
		s.clearBuffer()
		s.bufferObservedSincePrompt = false
		s.hasPendingProbe = false
		s.inputUnacknowledged = false
		s.abandonPendingProbeResync()
		s.hasAttribution = false
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}
	recovered := s.synchronization == StateDesynchronized
	s.synchronization = StateSynchronized
	s.foreground = ForegroundIdle
	s.promptReady = true
	s.buffer = emptyBufferSnapshot()
	s.hasBuffer = true
	s.bufferGeneration = s.inputGeneration
	s.hasBufferGeneration = true
	s.bufferObservedSincePrompt = false
	s.hasPendingProbe = false
	s.inputUnacknowledged = false
	s.hasAttribution = false
	return StateUpdate{kind: UpdatePromptReady, recovered: recovered}
}

func (s *ShellSessionState) announceCapability(announcement CapabilityAnnouncement) StateUpdate {
	s.invalidateEditingAuthority()
	s.hasPendingProbe = false
	s.probeResyncRequired = false
	s.hasPendingProbeResync = false
	s.capability = announcement.capability
	s.confirmedProbeNonce = announcement.lastProbeNonce
	s.hasConfirmedProbeNonce = announcement.hasLastNonce
	return StateUpdate{kind: UpdateCapabilityChanged, capability: announcement.capability}
}

func (s *ShellSessionState) applyProbeResync(response ProbeResyncResponse) StateUpdate {
	if !s.hasPendingProbeResync || response.requestID != s.pendingProbeResync {
		return StateUpdate{
			kind: UpdateProbeResyncRejected,
			probeRejection: ProbeResyncRejection{
				expected:    s.pendingProbeResync,
				hasExpected: s.hasPendingProbeResync,
				received:    response.requestID,
			},
		}
	}
	s.confirmedProbeNonce = response.lastProbeNonce
	s.hasConfirmedProbeNonce = true
	s.hasPendingProbe = false
	s.probeResyncRequired = false
	s.hasPendingProbeResync = false
	s.invalidateEditingAuthority()
	return StateUpdate{kind: UpdateProbeResynchronized, probeResync: response}
}

func (s *ShellSessionState) correlateSnapshot(buffer BufferSnapshot) (SnapshotRejection, bool) {
	switch s.capability {
	case BufferSyncProbe:
		if !buffer.hasProbe {
			return SnapshotRejection{kind: SnapshotMissingProbeNonce}, false
		}
		if !s.hasPendingProbe {
			if s.hasConfirmedProbeNonce && buffer.probeNonce.value > s.confirmedProbeNonce.value {
				s.probeResyncRequired = true
			}
			return SnapshotRejection{
				kind: SnapshotProbeNonceMismatch, nonce: buffer.probeNonce,
			}, false
		}
		expected := s.pendingProbe.nonce
		requested := s.pendingProbe.generation
		if buffer.probeNonce != expected {
			if buffer.probeNonce.value > expected.value {
				s.hasPendingProbe = false
				s.probeResyncRequired = true
			}
			return SnapshotRejection{
				kind:        SnapshotProbeNonceMismatch,
				nonce:       buffer.probeNonce,
				expected:    expected,
				hasExpected: true,
			}, false
		}
		s.hasPendingProbe = false
		s.confirmedProbeNonce = buffer.probeNonce
		s.hasConfirmedProbeNonce = true
		if requested != s.inputGeneration {
			return SnapshotRejection{
				kind:      SnapshotStaleProbeGeneration,
				requested: requested,
				current:   s.inputGeneration,
			}, false
		}
		return SnapshotRejection{}, true
	case BufferSyncNative:
		if buffer.hasProbe {
			return SnapshotRejection{
				kind: SnapshotUnexpectedProbeNonce, nonce: buffer.probeNonce,
			}, false
		}
		if s.inputUnacknowledged {
			return SnapshotRejection{
				kind: SnapshotUncorrelatedNative, current: s.inputGeneration,
			}, false
		}
	}
	return SnapshotRejection{}, true
}

func (s *ShellSessionState) startCommand(preexec SubmittedCommand, hasPreexec bool) StateUpdate {
	s.abandonPendingProbeResync()
	if s.foreground == ForegroundRunning {
		s.invalidateAuthority()
		return StateUpdate{
			kind: UpdateLifecycleRejected, lifecycleErr: LifecycleDuplicateCommandStart,
		}
	}
	s.inputUnacknowledged = false
	if s.foreground == ForegroundUnknown || !s.promptReady {
		s.foreground = ForegroundRunning
		s.promptReady = false
		s.buffer = BufferSnapshot{}
		s.hasBuffer = false
		s.bufferObservedSincePrompt = false
		s.hasAttribution = false
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}

	var snapshot SubmittedCommand
	hasSnapshot := false
	if s.bufferObservedSincePrompt && s.hasBuffer {
		snapshot = SubmittedCommand{bytes: bytes.Clone(s.buffer.bytes)}
		hasSnapshot = true
	}
	start := CommandStartUpdate{}
	if hasSnapshot && hasPreexec {
		start.preexecMatchesSnapshot = bytes.Equal(snapshot.bytes, preexec.bytes)
		start.hasPreexecMatch = true
	}
	if hasPreexec {
		s.attribution = attribution{command: preexec, source: AttributionLifecycleFrame}
		s.hasAttribution = true
		start.source = AttributionLifecycleFrame
		start.hasSource = true
	} else {
		s.hasAttribution = false
	}
	s.foreground = ForegroundRunning
	s.promptReady = false
	s.clearBuffer()
	s.bufferObservedSincePrompt = false
	s.hasPendingProbe = false
	return StateUpdate{kind: UpdateCommandStarted, commandStart: start}
}

func (s *ShellSessionState) stopCommand(status ShellExitStatus) StateUpdate {
	if s.foreground == ForegroundIdle {
		s.invalidateAuthority()
		return StateUpdate{
			kind: UpdateLifecycleRejected, lifecycleErr: LifecycleCommandStopWithoutStart,
		}
	}
	if s.foreground == ForegroundUnknown {
		s.foreground = ForegroundIdle
		s.promptReady = false
		s.hasAttribution = false
		return StateUpdate{kind: UpdateLifecycleSuppressed}
	}
	s.foreground = ForegroundIdle
	s.promptReady = false
	s.clearBuffer()
	s.lastStatus = status
	s.hasLastStatus = true
	if !s.hasAttribution {
		return StateUpdate{kind: UpdateCommandStoppedWithoutAttribution, status: status}
	}
	completed := CompletedCommand{
		command: s.attribution.command, status: status, source: s.attribution.source,
	}
	s.hasAttribution = false
	return StateUpdate{kind: UpdateCommandStopped, completed: completed}
}

func (s *ShellSessionState) abandonPendingProbeResync() {
	if s.hasPendingProbeResync {
		s.hasPendingProbeResync = false
		s.probeResyncRequired = true
	}
}

func (s *ShellSessionState) invalidateAuthority() {
	s.synchronization = StateDesynchronized
	s.foreground = ForegroundUnknown
	s.promptReady = false
	s.clearBuffer()
	s.bufferObservedSincePrompt = false
	s.hasPendingProbe = false
	s.abandonPendingProbeResync()
	s.hasAttribution = false
}

func (s *ShellSessionState) invalidateEditingAuthority() {
	s.synchronization = StateDesynchronized
	s.clearBuffer()
	s.bufferObservedSincePrompt = false
	s.hasAttribution = false
}

func (s *ShellSessionState) clearBuffer() {
	s.buffer = BufferSnapshot{}
	s.hasBuffer = false
	s.hasBufferGeneration = false
}

// String returns a representation that redacts shell buffer and command content.
func (s ShellSessionState) String() string {
	pending := "None"
	if s.hasPendingProbe {
		pending = fmt.Sprintf("Some(%s)", s.pendingProbe.nonce)
	}
	return fmt.Sprintf(
		"ShellSessionState { epoch: %d, synchronization: %d, foreground: %d, prompt_ready: %t, capability: %d, pending_probe_nonce: %s, buffer: %s, input_generation: %d, buffer_observed_since_prompt: %t, has_attribution: %t }",
		s.epoch.value,
		s.synchronization,
		s.foreground,
		s.promptReady,
		s.capability,
		pending,
		func() string {
			if s.hasBuffer {
				return s.buffer.String()
			}
			return "None"
		}(),
		s.inputGeneration.sequence,
		s.bufferObservedSincePrompt,
		s.hasAttribution,
	)
}

// GoString returns a representation that redacts shell buffer and command content.
func (s ShellSessionState) GoString() string { return s.String() }
