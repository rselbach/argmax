// Package session reduces terminal input and shell events into ordered inert
// effects for one interactive shell session. The reducer performs no I/O.
package session

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/coordinator"
	"github.com/rselbach/argmax/internal/inputrouter"
	"github.com/rselbach/argmax/internal/selection"
	"github.com/rselbach/argmax/internal/shellcontrol"
	"github.com/rselbach/argmax/internal/shellevents"
)

// MaxSessionEffects is the largest effect batch produced by one bounded input
// reduction.
const MaxSessionEffects = inputrouter.MaxRouteBatchEvents*7 + 2

// Mode selects the completion provider set for the current session.
type Mode uint8

const (
	// ModeSpec selects specification and local-context completion providers.
	ModeSpec Mode = iota + 1
	// ModeHistory selects shell-history completion providers.
	ModeHistory
)

// String returns the stable mode label.
func (m Mode) String() string {
	switch m {
	case ModeSpec:
		return "Spec"
	case ModeHistory:
		return "History"
	default:
		return fmt.Sprintf("Mode(%d)", m)
	}
}

// ReplacementErrorKind classifies an invalid shell-buffer replacement.
type ReplacementErrorKind uint8

const (
	// ReplacementTooLarge rejects an oversized shell-buffer replacement.
	ReplacementTooLarge ReplacementErrorKind = iota + 1
	// ReplacementInvalidCursor rejects malformed UTF-8 or an invalid cursor.
	ReplacementInvalidCursor
)

// ReplacementError describes invalid replacement structure without retaining
// replacement content.
type ReplacementError struct {
	kind              ReplacementErrorKind
	bytes, limit      int
	cursor, lineBytes int
}

// Kind returns the replacement rejection class.
func (e *ReplacementError) Kind() ReplacementErrorKind { return e.kind }

// Bytes returns the rejected replacement length.
func (e *ReplacementError) Bytes() int { return e.bytes }

// Limit returns the applicable byte bound.
func (e *ReplacementError) Limit() int { return e.limit }

// Cursor returns the rejected cursor offset.
func (e *ReplacementError) Cursor() int { return e.cursor }

// LineBytes returns the replacement length associated with the cursor.
func (e *ReplacementError) LineBytes() int { return e.lineBytes }

// Error describes the invalid replacement without exposing its text.
func (e *ReplacementError) Error() string {
	if e.kind == ReplacementTooLarge {
		return fmt.Sprintf("shell replacement is %d bytes; limit is %d", e.bytes, e.limit)
	}
	return fmt.Sprintf("shell replacement cursor %d is invalid for %d bytes", e.cursor, e.lineBytes)
}

// BufferReplacement is a validated full replacement for the shell-native edit
// buffer.
type BufferReplacement struct {
	text   string
	cursor int
}

// NewBufferReplacement validates and compacts an exact UTF-8 replacement.
func NewBufferReplacement(text string, cursor int) (BufferReplacement, error) {
	if len(text) > coordinator.MaxQueryLineBytes {
		return BufferReplacement{}, &ReplacementError{
			kind: ReplacementTooLarge, bytes: len(text), limit: coordinator.MaxQueryLineBytes,
		}
	}
	if cursor < 0 || cursor > len(text) || !utf8.ValidString(text) ||
		(cursor < len(text) && !utf8.RuneStart(text[cursor])) {
		return BufferReplacement{}, &ReplacementError{
			kind: ReplacementInvalidCursor, cursor: cursor, lineBytes: len(text),
		}
	}
	return BufferReplacement{text: strings.Clone(text), cursor: cursor}, nil
}

// Bytes returns a copy of the replacement text as bytes.
func (r BufferReplacement) Bytes() []byte { return []byte(r.text) }

// Text returns the replacement UTF-8 text.
func (r BufferReplacement) Text() string { return r.text }

// Cursor returns the replacement cursor byte offset.
func (r BufferReplacement) Cursor() int { return r.cursor }

// Len returns the replacement byte length.
func (r BufferReplacement) Len() int { return len(r.text) }

// Empty reports whether the replacement text is empty.
func (r BufferReplacement) Empty() bool { return r.text == "" }

// String returns replacement structure without exposing text.
func (r BufferReplacement) String() string {
	return fmt.Sprintf("BufferReplacement { byte_count: %d, cursor: %d }", len(r.text), r.cursor)
}

// GoString returns replacement structure without exposing text.
func (r BufferReplacement) GoString() string { return r.String() }

// FaultKind identifies a closed reducer failure.
type FaultKind uint8

const (
	// FaultInputGenerationExhausted closes input authority before generation wraps.
	FaultInputGenerationExhausted FaultKind = iota + 1
	// FaultInputBoundaryExhausted closes input authority before boundary count wraps.
	FaultInputBoundaryExhausted
	// FaultQueryStart reports an authoritative query construction failure.
	FaultQueryStart
	// FaultProbe reports an unrecoverable buffer-probe request failure.
	FaultProbe
	// FaultProbeResync reports an unrecoverable probe-resynchronization failure.
	FaultProbeResync
)

// Fault is a closed reducer failure. Variant accessors expose only the matching
// typed error.
type Fault struct {
	kind        FaultKind
	query       *coordinator.QueryStartError
	probe       *shellevents.ProbeRequestError
	probeResync *shellevents.ProbeResyncRequestError
}

// Kind returns the closed failure class.
func (f Fault) Kind() FaultKind { return f.kind }

// QueryStartError returns query failure details for FaultQueryStart.
func (f Fault) QueryStartError() (*coordinator.QueryStartError, bool) {
	return f.query, f.kind == FaultQueryStart
}

// ProbeError returns probe failure details for FaultProbe.
func (f Fault) ProbeError() (*shellevents.ProbeRequestError, bool) {
	return f.probe, f.kind == FaultProbe
}

// ProbeResyncError returns resynchronization details for FaultProbeResync.
func (f Fault) ProbeResyncError() (*shellevents.ProbeResyncRequestError, bool) {
	return f.probeResync, f.kind == FaultProbeResync
}

// Error describes the closed reducer failure.
func (f Fault) Error() string {
	switch f.kind {
	case FaultInputGenerationExhausted:
		return "local shell-input generation is exhausted"
	case FaultInputBoundaryExhausted:
		return "queued shell-input boundary space is exhausted"
	case FaultQueryStart:
		return f.query.Error()
	case FaultProbe:
		return f.probe.Error()
	case FaultProbeResync:
		return f.probeResync.Error()
	default:
		return "unknown session fault"
	}
}

// Unwrap returns variant-specific failure details, if any.
func (f Fault) Unwrap() error {
	switch f.kind {
	case FaultQueryStart:
		return f.query
	case FaultProbe:
		return f.probe
	case FaultProbeResync:
		return f.probeResync
	default:
		return nil
	}
}

// EffectKind identifies one inert non-I/O request.
type EffectKind uint8

const (
	// EffectForwardInput requests immediate input forwarding to the child shell.
	EffectForwardInput EffectKind = iota + 1
	// EffectClearOverlay requests removal of owned terminal presentation.
	EffectClearOverlay
	// EffectRefreshOverlay requests rendering from current selection state.
	EffectRefreshOverlay
	// EffectReplaceBuffer requests an inert shell-native buffer replacement.
	EffectReplaceBuffer
	// EffectRequestBufferSync requests an authoritative shell-buffer snapshot.
	EffectRequestBufferSync
	// EffectRequestProbeResync requests shell probe-counter resynchronization.
	EffectRequestProbeResync
	// EffectCancelProbeResync cancels a superseded resynchronization deadline.
	EffectCancelProbeResync
	// EffectStartQuery grants provider work authority for one generation.
	EffectStartQuery
	// EffectModeChanged reports a completed provider-mode transition.
	EffectModeChanged
	// EffectFault reports a closed reducer failure.
	EffectFault
)

// Effect is a typed tagged inert value. Accessors return data only for their
// matching kind and copy mutable byte slices.
type Effect struct {
	kind           EffectKind
	input          []byte
	replacement    BufferReplacement
	nonce          shellevents.SnapshotNonce
	resyncRequest  shellcontrol.ProbeResyncRequestID
	mode           Mode
	aliasExpansion bool
	work           coordinator.QueryWork
	fault          Fault
}

// Kind returns the effect tag.
func (e Effect) Kind() EffectKind { return e.kind }

// ForwardInput returns copied child input for EffectForwardInput.
func (e Effect) ForwardInput() ([]byte, bool) {
	return bytes.Clone(e.input), e.kind == EffectForwardInput
}

// Replacement returns replacement data for EffectReplaceBuffer.
func (e Effect) Replacement() (BufferReplacement, bool) {
	return e.replacement, e.kind == EffectReplaceBuffer
}

// BufferSyncNonce returns correlation data for EffectRequestBufferSync.
func (e Effect) BufferSyncNonce() (shellevents.SnapshotNonce, bool) {
	return e.nonce, e.kind == EffectRequestBufferSync
}

// ProbeResyncRequest returns correlation data for EffectRequestProbeResync.
func (e Effect) ProbeResyncRequest() (shellcontrol.ProbeResyncRequestID, bool) {
	return e.resyncRequest, e.kind == EffectRequestProbeResync
}

// CancelledProbeResyncRequest returns correlation data for EffectCancelProbeResync.
func (e Effect) CancelledProbeResyncRequest() (shellcontrol.ProbeResyncRequestID, bool) {
	return e.resyncRequest, e.kind == EffectCancelProbeResync
}

// Query returns mode, alias-expansion intent, and work for EffectStartQuery.
func (e Effect) Query() (Mode, bool, coordinator.QueryWork, bool) {
	return e.mode, e.aliasExpansion, e.work, e.kind == EffectStartQuery
}

// ChangedMode returns the new mode for EffectModeChanged.
func (e Effect) ChangedMode() (Mode, bool) { return e.mode, e.kind == EffectModeChanged }

// Fault returns failure details for EffectFault.
func (e Effect) Fault() (Fault, bool) { return e.fault, e.kind == EffectFault }

// String returns content-redacted effect state.
func (e Effect) String() string {
	switch e.kind {
	case EffectForwardInput:
		return fmt.Sprintf("ForwardInput { byte_count: %d }", len(e.input))
	case EffectClearOverlay:
		return "ClearOverlay"
	case EffectRefreshOverlay:
		return "RefreshOverlay"
	case EffectReplaceBuffer:
		return fmt.Sprintf("ReplaceBuffer(%s)", e.replacement)
	case EffectRequestBufferSync:
		return fmt.Sprintf("RequestBufferSync(%s)", e.nonce)
	case EffectRequestProbeResync:
		return fmt.Sprintf("RequestProbeResync(%s)", e.resyncRequest)
	case EffectCancelProbeResync:
		return fmt.Sprintf("CancelProbeResync(%s)", e.resyncRequest)
	case EffectStartQuery:
		return fmt.Sprintf("StartQuery { mode: %s, alias_expansion: %t, work: %s }", e.mode, e.aliasExpansion, e.work)
	case EffectModeChanged:
		return fmt.Sprintf("ModeChanged(%s)", e.mode)
	case EffectFault:
		return fmt.Sprintf("Fault(%d)", e.fault.kind)
	default:
		return fmt.Sprintf("Effect(%d)", e.kind)
	}
}

// GoString returns content-redacted effect state.
func (e Effect) GoString() string { return e.String() }

// EffectBatch is a bounded ordered group of inert effects.
type EffectBatch struct{ effects []Effect }

// Effects returns a copy of the ordered effects.
func (b EffectBatch) Effects() []Effect { return slices.Clone(b.effects) }

// Len returns the number of ordered effects.
func (b EffectBatch) Len() int { return len(b.effects) }

// Empty reports whether no effects were produced.
func (b EffectBatch) Empty() bool { return len(b.effects) == 0 }

// String returns content-redacted ordered effect state.
func (b EffectBatch) String() string {
	return fmt.Sprintf("EffectBatch { effect_count: %d, effects: %#v }", len(b.effects), b.effects)
}

// GoString returns content-redacted ordered effect state.
func (b EffectBatch) GoString() string { return b.String() }
func (b *EffectBatch) push(effect Effect) {
	b.effects = append(b.effects, effect)
}
func (b *EffectBatch) forward(input []byte) {
	if len(input) == 0 {
		return
	}
	if last := len(b.effects) - 1; last >= 0 && b.effects[last].kind == EffectForwardInput &&
		len(b.effects[last].input)+len(input) <= inputrouter.MaxRouteBatchEventBytes {
		b.effects[last].input = append(b.effects[last].input, input...)
		return
	}
	b.push(Effect{kind: EffectForwardInput, input: bytes.Clone(input)})
}

// InputReduction pairs consumed caller bytes with ordered effects.
type InputReduction struct {
	consumedBytes int
	effects       EffectBatch
}

// ConsumedBytes returns caller bytes consumed by this reduction.
func (r InputReduction) ConsumedBytes() int { return r.consumedBytes }

// Effects returns ordered inert effects from consumed input.
func (r InputReduction) Effects() EffectBatch { return r.effects }

// String returns content-redacted reduction state.
func (r InputReduction) String() string {
	return fmt.Sprintf("InputReduction { consumed_bytes: %d, effects: %s }", r.consumedBytes, r.effects)
}

// GoString returns content-redacted reduction state.
func (r InputReduction) GoString() string { return r.String() }

// PresentationReduction pairs coordinator disposition with derived effects.
type PresentationReduction struct {
	outcome coordinator.PresentationOutcome
	effects EffectBatch
}

// Outcome returns the coordinator presentation disposition.
func (r PresentationReduction) Outcome() coordinator.PresentationOutcome { return r.outcome }

// Effects returns ordered effects derived from presentation state.
func (r PresentationReduction) Effects() EffectBatch { return r.effects }

// String returns content-redacted presentation state.
func (r PresentationReduction) String() string {
	return fmt.Sprintf("PresentationReduction { outcome: %d, effects: %s }", r.outcome.Kind(), r.effects)
}

// GoString returns content-redacted presentation state.
func (r PresentationReduction) GoString() string { return r.String() }

// BuildErrorKind classifies invalid reducer construction or reconfiguration.
type BuildErrorKind uint8

const (
	// BuildInput rejects invalid or ambiguous keybindings.
	BuildInput BuildErrorKind = iota + 1
	// BuildCompletion rejects invalid completion-coordinator configuration.
	BuildCompletion
	// BuildCWDTooLarge rejects an oversized initial working directory.
	BuildCWDTooLarge
	// BuildCWDNotAbsolute rejects a non-absolute initial working directory.
	BuildCWDNotAbsolute
	// BuildInvalidMode rejects an unknown initial provider mode.
	BuildInvalidMode
)

// BuildError describes invalid reducer setup without retaining sensitive data.
type BuildError struct {
	kind         BuildErrorKind
	input        *inputrouter.InputRouterError
	registration *coordinator.RegistrationError
	bytes, limit int
	mode         Mode
}

// Kind returns the reducer construction failure class.
func (e *BuildError) Kind() BuildErrorKind { return e.kind }

// Error describes invalid reducer setup without exposing sensitive content.
func (e *BuildError) Error() string {
	switch e.kind {
	case BuildInput:
		return "invalid input bindings: " + e.input.Error()
	case BuildCompletion:
		return "invalid completion setup: " + e.registration.Error()
	case BuildCWDTooLarge:
		return fmt.Sprintf("session cwd is %d bytes; limit is %d", e.bytes, e.limit)
	case BuildCWDNotAbsolute:
		return "session cwd must be absolute"
	case BuildInvalidMode:
		return fmt.Sprintf("initial session mode %d is invalid", e.mode)
	default:
		return "invalid session setup"
	}
}

// Unwrap returns nested input or completion setup details, if any.
func (e *BuildError) Unwrap() error {
	if e.kind == BuildInput {
		return e.input
	}
	if e.kind == BuildCompletion {
		return e.registration
	}
	return nil
}

// CWDUpdateErrorKind classifies a rejected working-directory update.
type CWDUpdateErrorKind uint8

const (
	// CWDTooLarge rejects an oversized working-directory update.
	CWDTooLarge CWDUpdateErrorKind = iota + 1
	// CWDNotAbsolute rejects a non-absolute working-directory update.
	CWDNotAbsolute
)

// CWDUpdateError rejects a CWD atomically without retaining path content.
type CWDUpdateError struct {
	kind         CWDUpdateErrorKind
	bytes, limit int
}

// Kind returns the working-directory rejection class.
func (e *CWDUpdateError) Kind() CWDUpdateErrorKind { return e.kind }

// Bytes returns the rejected path length.
func (e *CWDUpdateError) Bytes() int { return e.bytes }

// Limit returns the applicable byte bound.
func (e *CWDUpdateError) Limit() int { return e.limit }

// Error describes the rejection without exposing path content.
func (e *CWDUpdateError) Error() string {
	if e.kind == CWDTooLarge {
		return fmt.Sprintf("session cwd is %d bytes; limit is %d", e.bytes, e.limit)
	}
	return "session cwd must be absolute"
}

type replacementKind uint8

const (
	replacementAcceptance replacementKind = iota + 1
	replacementAliasExpansion
	replacementGhost
	replacementHistoryPreview
	replacementHistoryRestore
)

type pendingReplacement struct {
	replacement BufferReplacement
	kind        replacementKind
}

// Reducer is the pure controller for one wrapped interactive shell session.
type Reducer struct {
	input                  *inputrouter.InputRouter
	shell                  *shellevents.ShellSessionState
	completion             *coordinator.CompletionCoordinator
	cwd                    []byte
	mode                   Mode
	pendingReplacement     *pendingReplacement
	historyOrigin          *BufferReplacement
	historyPreviewActive   bool
	recallHistoryWhenReady bool
	aliasExpansionPending  bool
	pasteActive            bool
	probeNeeded            bool
	inputBoundaryFence     bool
	queuedInputBoundaries  uint64
	fencedInputPending     bool
	fencePromptObserved    bool
	fenceSyncNonce         shellevents.SnapshotNonce
	hasFenceSyncNonce      bool
	queryRestartDeferred   bool
}

// New creates a reducer in specification mode.
func New(epoch shellevents.StreamEpoch, toggleMode, toggleMenu []byte, providers []string, uiMaxSuggestions int, cwd []byte) (*Reducer, error) {
	return NewWithMode(epoch, toggleMode, toggleMenu, providers, uiMaxSuggestions, cwd, ModeSpec)
}

// NewWithMode creates a reducer with an explicitly resolved initial mode.
func NewWithMode(epoch shellevents.StreamEpoch, toggleMode, toggleMenu []byte, providers []string, uiMaxSuggestions int, cwd []byte, initialMode Mode) (*Reducer, error) {
	if initialMode != ModeSpec && initialMode != ModeHistory {
		return nil, &BuildError{kind: BuildInvalidMode, mode: initialMode}
	}
	validatedCWD, err := validateCWD(cwd)
	if err != nil {
		if err.kind == CWDTooLarge {
			return nil, &BuildError{kind: BuildCWDTooLarge, bytes: err.bytes, limit: err.limit}
		}
		return nil, &BuildError{kind: BuildCWDNotAbsolute}
	}
	input, inputErr := inputrouter.New(toggleMode, toggleMenu)
	if inputErr != nil {
		var typed *inputrouter.InputRouterError
		errors.As(inputErr, &typed)
		return nil, &BuildError{kind: BuildInput, input: typed}
	}
	completionCoordinator, completionErr := coordinator.New(providers, uiMaxSuggestions)
	if completionErr != nil {
		var typed *coordinator.RegistrationError
		errors.As(completionErr, &typed)
		return nil, &BuildError{kind: BuildCompletion, registration: typed}
	}
	return &Reducer{
		input: input, shell: shellevents.NewShellSessionState(epoch),
		completion: completionCoordinator, cwd: validatedCWD, mode: initialMode,
	}, nil
}

// Mode returns the active completion provider mode.
func (r *Reducer) Mode() Mode { return r.mode }

// Shell returns a snapshot of authoritative shell state.
func (r *Reducer) Shell() shellevents.ShellSessionState { return *r.shell }

// Selection returns a snapshot of bounded suggestion selection state.
func (r *Reducer) Selection() selection.SelectionState { return r.completion.Selection() }

// ActiveQuery returns the current authoritative completion query, if any.
func (r *Reducer) ActiveQuery() (completion.CompletionQuery, bool) {
	return r.completion.ActiveQuery()
}

// CWD returns a copy of the opaque working-directory bytes.
func (r *Reducer) CWD() []byte { return bytes.Clone(r.cwd) }

// QueryRestartDeferred reports whether authority must be queried when safe.
func (r *Reducer) QueryRestartDeferred() bool { return r.queryRestartDeferred }

// ReplacementPending reports whether shell confirmation is outstanding.
func (r *Reducer) ReplacementPending() bool { return r.pendingReplacement != nil }

// Close cancels active provider work. It is safe to call more than once.
func (r *Reducer) Close() error { return r.completion.Close() }

// UpdateCWD atomically changes the path for new provider queries.
func (r *Reducer) UpdateCWD(cwd []byte) (EffectBatch, error) {
	validated, err := validateCWD(cwd)
	if err != nil {
		return EffectBatch{}, err
	}
	r.cwd = validated
	var effects EffectBatch
	r.clearCompletion(&effects)
	if r.actionsAreSafe() {
		r.startQueryFromAuthority(&effects)
	} else {
		r.queryRestartDeferred = true
	}
	return effects, nil
}

// Reconfigure applies keybindings and the UI limit. A false applied result asks
// the caller to retry after a retained configurable prefix resolves.
func (r *Reducer) Reconfigure(toggleMode, toggleMenu []byte, uiMaxSuggestions int) (effects EffectBatch, applied bool, err error) {
	configured, inputErr := r.input.Reconfigure(toggleMode, toggleMenu)
	if inputErr != nil {
		var typed *inputrouter.InputRouterError
		errors.As(inputErr, &typed)
		return EffectBatch{}, false, &BuildError{kind: BuildInput, input: typed}
	}
	if !configured {
		return EffectBatch{}, false, nil
	}
	if completionErr := r.completion.ReconfigureUILimit(uiMaxSuggestions); completionErr != nil {
		var typed *coordinator.RegistrationError
		errors.As(completionErr, &typed)
		return EffectBatch{}, false, &BuildError{kind: BuildCompletion, registration: typed}
	}
	r.clearCompletion(&effects)
	if r.actionsAreSafe() {
		r.startQueryFromAuthority(&effects)
	} else {
		r.queryRestartDeferred = true
	}
	return effects, true, nil
}

// RouteInput incrementally reduces caller-owned terminal input.
func (r *Reducer) RouteInput(input []byte) InputReduction {
	return r.reduceRouteBatch(r.input.Route(input))
}

// FlushInput forwards an ambiguous retained input prefix without ending input.
func (r *Reducer) FlushInput() InputReduction {
	return r.reduceRouteBatch(r.input.FlushPending())
}

// FinishInput forwards retained input and closes input routing.
func (r *Reducer) FinishInput() InputReduction {
	return r.reduceRouteBatch(r.input.Finish())
}

// ApplyShellFrame applies one ordered decoded shell frame.
func (r *Reducer) ApplyShellFrame(frame shellevents.DecodedFrame) (shellevents.StateUpdate, EffectBatch) {
	promptObserved := false
	if event, ok := frame.Event(); ok {
		promptObserved = event.Event().Kind() == shellevents.EventPromptReady
	}
	pendingResync, hadPendingResync := r.shell.PendingProbeResyncRequestID()
	update := r.shell.Apply(frame)
	var effects EffectBatch
	currentResync, hasCurrentResync := r.shell.PendingProbeResyncRequestID()
	response, isResynchronized := update.ProbeResync()
	if hadPendingResync && (!hasCurrentResync || currentResync != pendingResync) &&
		(!isResynchronized || response.RequestID() != pendingResync) {
		effects.push(Effect{kind: EffectCancelProbeResync, resyncRequest: pendingResync})
	}

	switch update.Kind() {
	case shellevents.UpdateBufferSynchronized:
		r.handleAuthoritativeUpdate(false, &effects)
	case shellevents.UpdatePromptReady:
		r.handleAuthoritativeUpdate(true, &effects)
	case shellevents.UpdateWorkingDirectoryChanged:
		directory, _ := update.WorkingDirectory()
		path := directory.Bytes()
		if !bytes.Equal(r.cwd, path) {
			r.cwd = path
			r.clearCompletion(&effects)
			r.queryRestartDeferred = true
		}
	case shellevents.UpdateReloadRequested:
		r.clearCompletion(&effects)
		r.clearHistoryPreviewAuthority()
	case shellevents.UpdateSnapshotRejected:
		r.clearCompletion(&effects)
		r.clearHistoryPreviewAuthority()
		r.pendingReplacement = nil
		if r.shell.ProbeResyncRequired() {
			r.probeNeeded = true
		}
		r.issueProbeIfSafe(&effects, false)
	case shellevents.UpdateProbeResynchronized:
		r.issueProbeIfSafe(&effects, false)
	case shellevents.UpdateProbeResyncRejected:
	case shellevents.UpdateLifecycleSuppressed:
		switch {
		case promptObserved && r.shell.ProbeResyncRequired():
			r.pendingReplacement = nil
			r.clearHistoryPreviewAuthority()
			if r.inputBoundaryFence {
				r.handleSuppressedPrompt(&effects)
			} else {
				r.clearCompletion(&effects)
				r.probeNeeded = true
				r.issueProbeIfSafe(&effects, false)
			}
		case promptObserved && r.inputBoundaryFence:
			r.pendingReplacement = nil
			r.clearHistoryPreviewAuthority()
			r.handleSuppressedPrompt(&effects)
		default:
			r.clearCompletion(&effects)
			r.pendingReplacement = nil
			r.clearHistoryPreviewAuthority()
		}
	default:
		r.clearCompletion(&effects)
		r.pendingReplacement = nil
		r.clearHistoryPreviewAuthority()
		switch update.Kind() {
		case shellevents.UpdateCommandStarted, shellevents.UpdateCapabilityChanged,
			shellevents.UpdateFrameRejected, shellevents.UpdateLifecycleRejected,
			shellevents.UpdateStreamOrderRejected:
			r.fencePromptObserved = false
			r.hasFenceSyncNonce = false
		}
	}
	return update, effects
}

// ObserveShellOutput invalidates asynchronous UI before shell output.
func (r *Reducer) ObserveShellOutput() EffectBatch {
	var effects EffectBatch
	r.clearCompletion(&effects)
	r.clearHistoryPreviewAuthority()
	r.probeNeeded = true
	r.issueProbeIfSafe(&effects, false)
	return effects
}

// AcceptProviderBatch validates and retains one provider response.
func (r *Reducer) AcceptProviderBatch(batch completion.ProviderBatch) coordinator.BatchOutcome {
	return r.completion.AcceptBatch(batch)
}

// MergedCandidates returns unranked deduplicated candidates for a generation.
func (r *Reducer) MergedCandidates(generation uint64) ([]completion.Suggestion, error) {
	return r.completion.MergedCandidates(generation)
}

// ApplyRankedCandidates validates ranking and derives presentation effects.
func (r *Reducer) ApplyRankedCandidates(generation uint64, ranked []completion.Suggestion) PresentationReduction {
	outcome := r.completion.ApplyRanked(generation, ranked)
	var effects EffectBatch
	if outcome.Kind() == coordinator.PresentationApplied {
		if r.recallHistoryWhenReady {
			r.recallHistoryWhenReady = false
			if !r.previewSelectedHistory(&effects) {
				r.refreshOrClearOverlay(&effects)
			}
		} else {
			r.refreshOrClearOverlay(&effects)
		}
	}
	r.issueProbeIfSafe(&effects, false)
	return PresentationReduction{outcome: outcome, effects: effects}
}

// ApplyAliasExpansion applies one background-validated alias edit through the
// normal shell replacement protocol.
func (r *Reducer) ApplyAliasExpansion(generation uint64, edit completion.TextEdit) EffectBatch {
	var effects EffectBatch
	if !r.actionsAreSafe() {
		return effects
	}
	query, ok := r.completion.ActiveQuery()
	if !ok || query.Generation() != generation || edit.Start() < 0 ||
		edit.Start() > edit.End() || edit.End() > query.Cursor() ||
		!utf8Boundary(query.Line(), edit.Start()) || !utf8Boundary(query.Line(), edit.End()) {
		return effects
	}
	removed := edit.End() - edit.Start()
	cursor := query.Cursor() - removed
	if len(edit.Replacement()) > math.MaxInt-cursor {
		return effects
	}
	cursor += len(edit.Replacement())
	text, err := edit.Apply(query.Line())
	if err != nil {
		return effects
	}
	replacement, err := NewBufferReplacement(text, cursor)
	if err != nil {
		return effects
	}
	r.beginReplacement(replacement, replacementAliasExpansion, &effects)
	return effects
}

func (r *Reducer) reduceRouteBatch(batch inputrouter.RouteBatch) InputReduction {
	var effects EffectBatch
	for _, event := range batch.Events() {
		action, hasAction := event.Action()
		r.releaseUnconfirmedBoundariesForLocalToggle(action, hasAction, &effects)
		fencedBefore := r.inputBoundaryFence
		character, printable := action.Character()
		typedAliasSpace := hasAction && printable && character == ' ' && r.actionsAreSafe()
		handled := hasAction && r.handleAction(action, &effects)
		restoringBeforeEscape := hasAction && action.Kind() == inputrouter.ActionEscape &&
			r.pendingReplacement != nil && r.pendingReplacement.kind == replacementHistoryRestore
		forward := event.Forwarding() == inputrouter.ForwardImmediate ||
			event.Forwarding() == inputrouter.ForwardOnFallback && !handled
		input := event.Bytes()
		if !forward || len(input) == 0 {
			continue
		}
		kind := inputrouter.ActionDesynchronize
		if hasAction {
			kind = action.Kind()
		}
		if fencedBefore {
			r.noteFencedForward(kind, &effects)
		} else if kind != inputrouter.ActionEnter && kind != inputrouter.ActionCtrlC &&
			!restoringBeforeEscape && kind != inputrouter.ActionPasteStart && kind != inputrouter.ActionPasteEnd {
			r.observeForwardedEdit(&effects, typedAliasSpace)
		}
		effects.forward(input)
	}
	r.restartDeferredQueryIfSafe(&effects)
	r.issueProbeIfSafe(&effects, true)
	return InputReduction{consumedBytes: batch.ConsumedBytes(), effects: effects}
}

func (r *Reducer) handleAction(action inputrouter.InputAction, effects *EffectBatch) bool {
	if r.inputBoundaryFence {
		return false
	}
	switch action.Kind() {
	case inputrouter.ActionTab:
		return r.handleTab(effects)
	case inputrouter.ActionEnter, inputrouter.ActionCtrlC:
		r.handleInputBoundary(effects)
		return false
	case inputrouter.ActionEscape:
		return r.handleEscape(effects)
	case inputrouter.ActionArrowUp:
		return r.handleVerticalNavigation(false, effects)
	case inputrouter.ActionArrowDown:
		return r.handleVerticalNavigation(true, effects)
	case inputrouter.ActionArrowRight:
		return r.acceptGhost(effects)
	case inputrouter.ActionCtrlU:
		r.clearCompletion(effects)
		return false
	case inputrouter.ActionToggleMode:
		return r.toggleMode(effects)
	case inputrouter.ActionToggleMenu:
		return r.toggleMenu(effects)
	case inputrouter.ActionPasteStart:
		r.pasteActive = true
		r.clearCompletion(effects)
		return false
	case inputrouter.ActionPasteEnd:
		r.pasteActive = false
		return false
	case inputrouter.ActionDesynchronize:
		r.pasteActive = false
		r.clearCompletion(effects)
		r.clearHistoryPreviewAuthority()
		return false
	default:
		return false
	}
}

func (r *Reducer) handleTab(effects *EffectBatch) bool {
	selectionState := r.completion.Selection()
	if !r.actionsAreSafe() || !selectionState.LayerEnabled() {
		return false
	}
	if selectionState.IsVisible() {
		return r.replaceWithSelected(replacementAcceptance, effects)
	}
	if selectionState.CandidateCount() == 0 {
		return false
	}
	r.completion.NoteBufferChanged()
	effects.push(Effect{kind: EffectRefreshOverlay})
	return true
}

func (r *Reducer) handleInputBoundary(effects *EffectBatch) {
	r.completion.CancelActiveQuery()
	r.completion.DismissSuggestions()
	r.clearHistoryPreviewAuthority()
	r.pendingReplacement = nil
	r.inputBoundaryFence = true
	r.queuedInputBoundaries = 1
	r.fencedInputPending = false
	r.fencePromptObserved = false
	r.hasFenceSyncNonce = false
	effects.push(Effect{kind: EffectClearOverlay})
}

func (r *Reducer) handleEscape(effects *EffectBatch) bool {
	if r.mode == ModeHistory && r.historyPreviewActive && r.actionsAreSafe() {
		r.clearCompletion(effects)
		r.mode = ModeSpec
		r.historyPreviewActive = false
		r.recallHistoryWhenReady = false
		effects.push(Effect{kind: EffectModeChanged, mode: r.mode})
		if r.historyOrigin != nil {
			origin := *r.historyOrigin
			r.historyOrigin = nil
			r.beginReplacement(origin, replacementHistoryRestore, effects)
		}
		return false
	}
	r.completion.DismissSuggestions()
	effects.push(Effect{kind: EffectClearOverlay})
	return false
}

func (r *Reducer) handleVerticalNavigation(down bool, effects *EffectBatch) bool {
	if !r.actionsAreSafe() {
		return false
	}
	selectionState := r.completion.Selection()
	if !selectionState.IsVisible() {
		buffer, ok := r.shell.Buffer()
		if ok && buffer.Empty() {
			return r.enterHistoryForRecall(effects)
		}
		return false
	}
	if down {
		r.completion.SelectNext()
	} else {
		r.completion.SelectPrevious()
	}
	if r.mode == ModeHistory && r.previewSelectedHistory(effects) {
		return true
	}
	effects.push(Effect{kind: EffectRefreshOverlay})
	return true
}

func (r *Reducer) acceptGhost(effects *EffectBatch) bool {
	selectionState := r.completion.Selection()
	if !r.actionsAreSafe() || !selectionState.IsVisible() {
		return false
	}
	query, ok := r.completion.ActiveQuery()
	if !ok || query.Cursor() != len(query.Line()) {
		return false
	}
	candidate, ok := selectionState.Selected()
	if !ok {
		return false
	}
	result, err := candidate.ResultingLine(query)
	if err != nil {
		return false
	}
	suffix, ok := selection.GhostSuffix(query.Line(), result)
	if !ok || strings.IndexFunc(suffix, unicode.IsControl) >= 0 {
		return false
	}
	text := query.Line() + suffix
	replacement, err := NewBufferReplacement(text, len(text))
	if err != nil {
		return false
	}
	return r.beginReplacement(replacement, replacementGhost, effects)
}

func (r *Reducer) toggleMode(effects *EffectBatch) bool {
	if !r.actionsAreSafe() && !r.canRestorePendingHistory() {
		return false
	}
	r.clearCompletion(effects)
	switch r.mode {
	case ModeSpec:
		origin, ok := r.authoritativeReplacement()
		if !ok {
			return false
		}
		r.mode = ModeHistory
		r.historyOrigin = &origin
		r.historyPreviewActive = false
		r.recallHistoryWhenReady = false
		effects.push(Effect{kind: EffectModeChanged, mode: r.mode})
		r.startQueryFromAuthority(effects)
	case ModeHistory:
		restore := r.historyPreviewActive || r.pendingReplacement != nil &&
			r.pendingReplacement.kind == replacementHistoryPreview
		if restore && !r.shell.ProbeAvailable() {
			return true
		}
		r.mode = ModeSpec
		r.recallHistoryWhenReady = false
		effects.push(Effect{kind: EffectModeChanged, mode: r.mode})
		origin := r.historyOrigin
		r.historyOrigin = nil
		r.historyPreviewActive = false
		if restore {
			if origin != nil {
				r.beginReplacement(*origin, replacementHistoryRestore, effects)
			}
		} else {
			r.startQueryFromAuthority(effects)
		}
	}
	return true
}

func (r *Reducer) toggleMenu(effects *EffectBatch) bool {
	if !r.actionsAreSafe() {
		return false
	}
	r.completion.ToggleSuggestionLayer()
	r.refreshOrClearOverlay(effects)
	return true
}

func (r *Reducer) enterHistoryForRecall(effects *EffectBatch) bool {
	origin, ok := r.authoritativeReplacement()
	if !ok {
		return false
	}
	r.clearCompletion(effects)
	r.mode = ModeHistory
	r.historyOrigin = &origin
	r.historyPreviewActive = false
	r.recallHistoryWhenReady = true
	effects.push(Effect{kind: EffectModeChanged, mode: r.mode})
	r.startQueryFromAuthority(effects)
	return true
}

func (r *Reducer) previewSelectedHistory(effects *EffectBatch) bool {
	selectionState := r.completion.Selection()
	candidate, ok := selectionState.Selected()
	if r.mode != ModeHistory || !ok || candidate.Source() != completion.SourceHistory {
		return false
	}
	if r.historyOrigin == nil {
		if origin, ok := r.authoritativeReplacement(); ok {
			r.historyOrigin = &origin
		}
	}
	return r.replaceWithSelected(replacementHistoryPreview, effects)
}

func (r *Reducer) replaceWithSelected(kind replacementKind, effects *EffectBatch) bool {
	query, ok := r.completion.ActiveQuery()
	if !ok {
		return false
	}
	selectionState := r.completion.Selection()
	candidate, ok := selectionState.Selected()
	if !ok {
		return false
	}
	edit, err := candidate.ResolvedEdit(query.Line())
	if err != nil || len(edit.Replacement()) > math.MaxInt-edit.Start() {
		return false
	}
	text, err := edit.Apply(query.Line())
	if err != nil {
		return false
	}
	replacement, err := NewBufferReplacement(text, edit.Start()+len(edit.Replacement()))
	if err != nil {
		return false
	}
	return r.beginReplacement(replacement, kind, effects)
}

func (r *Reducer) beginReplacement(replacement BufferReplacement, kind replacementKind, effects *EffectBatch) bool {
	if _, err := r.shell.ObserveLocalInput(); err != nil {
		r.clearCompletion(effects)
		effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultInputGenerationExhausted}})
		return false
	}
	r.completion.CancelActiveQuery()
	r.completion.DismissSuggestions()
	r.probeNeeded = true
	if r.shell.Capability() != shellevents.BufferSyncProbe {
		effects.push(Effect{kind: EffectClearOverlay})
		return false
	}
	nonce, err := r.shell.BeginSyncProbe()
	if err != nil {
		var probeErr *shellevents.ProbeRequestError
		if !errors.As(err, &probeErr) {
			effects.push(Effect{kind: EffectClearOverlay})
			return false
		}
		switch probeErr.Kind() {
		case shellevents.ProbeAlreadyPending, shellevents.ProbeNotAtEditablePrompt,
			shellevents.ProbeResyncRequired, shellevents.ProbeResyncPending:
			effects.push(Effect{kind: EffectClearOverlay})
			return false
		default:
			r.probeNeeded = false
			effects.push(Effect{kind: EffectClearOverlay})
			effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultProbe, probe: probeErr}})
			return false
		}
	}
	r.probeNeeded = false
	r.pendingReplacement = &pendingReplacement{replacement: replacement, kind: kind}
	if kind == replacementAcceptance || kind == replacementAliasExpansion || kind == replacementGhost {
		r.clearHistoryPreviewAuthority()
	}
	effects.push(Effect{kind: EffectClearOverlay})
	effects.push(Effect{kind: EffectReplaceBuffer, replacement: replacement})
	effects.push(Effect{kind: EffectRequestBufferSync, nonce: nonce})
	return true
}

func (r *Reducer) observeForwardedEdit(effects *EffectBatch, typedAliasSpace bool) {
	if _, err := r.shell.ObserveLocalInput(); err != nil {
		effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultInputGenerationExhausted}})
	}
	r.clearCompletion(effects)
	r.aliasExpansionPending = typedAliasSpace
	r.pendingReplacement = nil
	r.probeNeeded = true
	r.clearHistoryPreviewAuthority()
}

func (r *Reducer) startQueryFromAuthority(effects *EffectBatch) {
	r.queryRestartDeferred = false
	r.completion.CancelActiveQuery()
	aliasExpansion := r.aliasExpansionPending
	r.aliasExpansionPending = false
	line, cursor, ok := r.queryTextAndCursor()
	if !ok {
		return
	}
	if r.mode == ModeSpec && line == "" {
		effects.push(Effect{kind: EffectClearOverlay})
		return
	}
	work, err := r.completion.StartQuery(line, cursor, r.cwd)
	if err == nil {
		effects.push(Effect{kind: EffectStartQuery, mode: r.mode, aliasExpansion: aliasExpansion, work: work})
		return
	}
	var queryErr *coordinator.QueryStartError
	if errors.As(err, &queryErr) {
		effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultQueryStart, query: queryErr}})
	}
}

func (r *Reducer) queryTextAndCursor() (string, int, bool) {
	if r.mode == ModeHistory && r.historyPreviewActive && r.historyOrigin != nil {
		return r.historyOrigin.text, r.historyOrigin.cursor, true
	}
	buffer, ok := r.shell.Buffer()
	if !ok {
		return "", 0, false
	}
	text, ok := buffer.StringValue()
	return text, buffer.Cursor(), ok
}

func (r *Reducer) authoritativeReplacement() (BufferReplacement, bool) {
	buffer, ok := r.shell.Buffer()
	if !ok {
		return BufferReplacement{}, false
	}
	text, ok := buffer.StringValue()
	if !ok {
		return BufferReplacement{}, false
	}
	replacement, err := NewBufferReplacement(text, buffer.Cursor())
	return replacement, err == nil
}

func (r *Reducer) handleAuthoritativeUpdate(prompt bool, effects *EffectBatch) {
	if !r.inputBoundaryFence {
		r.probeNeeded = false
		r.confirmOrDiscardReplacement()
		if prompt {
			r.clearHistoryPreviewAuthority()
		}
		r.startQueryFromAuthority(effects)
		return
	}
	r.pendingReplacement = nil
	if prompt {
		r.hasFenceSyncNonce = false
		if r.consumeQueuedBoundaryPrompt(effects) {
			return
		}
		r.fencePromptObserved = true
		if r.fencedInputPending {
			r.prepareFenceProbe(effects)
		} else {
			r.releaseInputBoundary(effects)
		}
		return
	}
	buffer, hasBuffer := r.shell.Buffer()
	receivedNonce, hasReceivedNonce := buffer.ProbeNonce()
	causallyNew := r.hasFenceSyncNonce && hasBuffer && hasReceivedNonce && receivedNonce == r.fenceSyncNonce
	if causallyNew && !r.fencedInputPending {
		r.releaseInputBoundary(effects)
	} else {
		r.clearCompletion(effects)
		if r.fencePromptObserved {
			r.probeNeeded = true
			r.issueProbeIfSafe(effects, false)
		}
	}
}

func (r *Reducer) handleSuppressedPrompt(effects *EffectBatch) {
	r.hasFenceSyncNonce = false
	if r.consumeQueuedBoundaryPrompt(effects) {
		return
	}
	r.fencePromptObserved = true
	if r.fencedInputPending {
		r.prepareFenceProbe(effects)
	} else {
		r.clearCompletion(effects)
		r.probeNeeded = true
		r.issueProbeIfSafe(effects, false)
	}
}

func (r *Reducer) consumeQueuedBoundaryPrompt(effects *EffectBatch) bool {
	if r.queuedInputBoundaries == 0 {
		return false
	}
	r.queuedInputBoundaries--
	r.fencePromptObserved = r.queuedInputBoundaries != 0
	r.hasFenceSyncNonce = false
	if r.queuedInputBoundaries == 0 {
		return false
	}
	r.probeNeeded = false
	r.clearCompletion(effects)
	return true
}

func (r *Reducer) releaseUnconfirmedBoundariesForLocalToggle(action inputrouter.InputAction, hasAction bool, effects *EffectBatch) {
	if !hasAction || action.Kind() != inputrouter.ActionToggleMode && action.Kind() != inputrouter.ActionToggleMenu ||
		!r.inputBoundaryFence || !r.fencePromptObserved || r.queuedInputBoundaries == 0 ||
		r.fencedInputPending || r.hasFenceSyncNonce || !r.shell.SuggestionsAllowed() {
		return
	}
	r.releaseInputBoundary(effects)
}

func (r *Reducer) prepareFenceProbe(effects *EffectBatch) {
	r.clearCompletion(effects)
	r.fencedInputPending = false
	if _, err := r.shell.ObserveLocalInput(); err != nil {
		effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultInputGenerationExhausted}})
		return
	}
	r.probeNeeded = true
	r.issueProbeIfSafe(effects, false)
}

func (r *Reducer) releaseInputBoundary(effects *EffectBatch) {
	r.inputBoundaryFence = false
	r.queuedInputBoundaries = 0
	r.fencedInputPending = false
	r.fencePromptObserved = false
	r.hasFenceSyncNonce = false
	r.probeNeeded = false
	r.startQueryFromAuthority(effects)
}

func (r *Reducer) noteFencedForward(action inputrouter.ActionKind, effects *EffectBatch) {
	inputBoundary := action == inputrouter.ActionEnter || action == inputrouter.ActionCtrlC
	if inputBoundary && r.shell.Foreground() == shellevents.ForegroundRunning {
		return
	}
	if r.hasFenceSyncNonce {
		r.hasFenceSyncNonce = false
		if _, err := r.shell.ObserveLocalInput(); err != nil {
			effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultInputGenerationExhausted}})
		}
		r.probeNeeded = true
	}
	if inputBoundary {
		if r.queuedInputBoundaries == math.MaxUint64 {
			r.fencedInputPending = true
			effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultInputBoundaryExhausted}})
			return
		}
		r.queuedInputBoundaries++
		r.fencedInputPending = false
		return
	}
	r.fencedInputPending = true
}

func (r *Reducer) confirmOrDiscardReplacement() {
	pending := r.pendingReplacement
	r.pendingReplacement = nil
	if pending == nil {
		return
	}
	buffer, ok := r.shell.Buffer()
	matches := ok && bytes.Equal(buffer.Bytes(), pending.replacement.Bytes()) && buffer.Cursor() == pending.replacement.cursor
	if pending.kind == replacementHistoryPreview {
		r.historyPreviewActive = matches
		if !matches {
			r.historyOrigin = nil
		}
	}
}

func (r *Reducer) clearHistoryPreviewAuthority() {
	r.historyOrigin = nil
	r.historyPreviewActive = false
	r.recallHistoryWhenReady = false
}
func (r *Reducer) actionsAreSafe() bool {
	return r.shell.SuggestionsAllowed() && r.pendingReplacement == nil && !r.pasteActive &&
		!r.input.IsBracketedPaste() && !r.inputBoundaryFence
}
func (r *Reducer) canRestorePendingHistory() bool {
	return r.mode == ModeHistory && r.historyOrigin != nil && r.shell.Foreground() == shellevents.ForegroundIdle &&
		r.pendingReplacement != nil && r.pendingReplacement.kind == replacementHistoryPreview
}
func (r *Reducer) clearCompletion(effects *EffectBatch) {
	r.completion.CancelActiveQuery()
	r.completion.DismissSuggestions()
	r.aliasExpansionPending = false
	effects.push(Effect{kind: EffectClearOverlay})
}
func (r *Reducer) refreshOrClearOverlay(effects *EffectBatch) {
	selectionState := r.completion.Selection()
	if r.actionsAreSafe() && selectionState.IsVisible() {
		effects.push(Effect{kind: EffectRefreshOverlay})
	} else {
		effects.push(Effect{kind: EffectClearOverlay})
	}
}
func (r *Reducer) restartDeferredQueryIfSafe(effects *EffectBatch) {
	if r.queryRestartDeferred && r.actionsAreSafe() {
		r.startQueryFromAuthority(effects)
	}
}

func (r *Reducer) issueProbeIfSafe(effects *EffectBatch, requireCompleteInput bool) {
	if !r.probeNeeded || r.pasteActive || r.input.IsBracketedPaste() ||
		requireCompleteInput && r.input.PendingLen() != 0 ||
		r.shell.Capability() != shellevents.BufferSyncProbe ||
		r.inputBoundaryFence && !r.fencePromptObserved || r.queuedInputBoundaries != 0 {
		return
	}
	if r.shell.ProbeResyncRequired() {
		request, err := r.shell.BeginProbeResync()
		if err == nil {
			effects.push(Effect{kind: EffectRequestProbeResync, resyncRequest: request})
			return
		}
		var resyncErr *shellevents.ProbeResyncRequestError
		if !errors.As(err, &resyncErr) {
			return
		}
		switch resyncErr.Kind() {
		case shellevents.ResyncAlreadyPending, shellevents.ResyncNotAtEditablePrompt:
		default:
			effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultProbeResync, probeResync: resyncErr}})
		}
		return
	}
	nonce, err := r.shell.BeginSyncProbe()
	if err == nil {
		r.probeNeeded = false
		if r.inputBoundaryFence {
			r.fenceSyncNonce = nonce
			r.hasFenceSyncNonce = true
			r.fencedInputPending = false
		}
		effects.push(Effect{kind: EffectRequestBufferSync, nonce: nonce})
		return
	}
	var probeErr *shellevents.ProbeRequestError
	if !errors.As(err, &probeErr) {
		return
	}
	switch probeErr.Kind() {
	case shellevents.ProbeAlreadyPending, shellevents.ProbeNotAtEditablePrompt:
	default:
		r.probeNeeded = false
		effects.push(Effect{kind: EffectFault, fault: Fault{kind: FaultProbe, probe: probeErr}})
	}
}

func validateCWD(cwd []byte) ([]byte, *CWDUpdateError) {
	if len(cwd) > coordinator.MaxQueryCWDBytes {
		return nil, &CWDUpdateError{kind: CWDTooLarge, bytes: len(cwd), limit: coordinator.MaxQueryCWDBytes}
	}
	if len(cwd) == 0 || cwd[0] != '/' {
		return nil, &CWDUpdateError{kind: CWDNotAbsolute}
	}
	return bytes.Clone(cwd), nil
}

func utf8Boundary(value string, index int) bool {
	return index >= 0 && index <= len(value) && (index == len(value) || utf8.RuneStart(value[index]))
}

// String returns structural reducer state with shell buffers, commands,
// replacements, CWD, and candidate text redacted.
func (r *Reducer) String() string {
	pendingKind := "None"
	if r.pendingReplacement != nil {
		pendingKind = fmt.Sprintf("Some(%d,%s)", r.pendingReplacement.kind, r.pendingReplacement.replacement)
	}
	historyOrigin := "None"
	if r.historyOrigin != nil {
		historyOrigin = r.historyOrigin.String()
	}
	return fmt.Sprintf("Reducer { input: %s, shell: %s, completion: %s, cwd_bytes: %d, mode: %s, pending_replacement: %s, history_origin: %s, history_preview_active: %t, recall_history_when_ready: %t, alias_expansion_pending: %t, paste_active: %t, probe_needed: %t, input_boundary_fence: %t, queued_input_boundaries: %d, fenced_input_pending: %t, fence_prompt_observed: %t, has_fence_sync_nonce: %t, query_restart_deferred: %t }", r.input, r.shell, r.completion, len(r.cwd), r.mode, pendingKind, historyOrigin, r.historyPreviewActive, r.recallHistoryWhenReady, r.aliasExpansionPending, r.pasteActive, r.probeNeeded, r.inputBoundaryFence, r.queuedInputBoundaries, r.fencedInputPending, r.fencePromptObserved, r.hasFenceSyncNonce, r.queryRestartDeferred)
}

// GoString returns structural reducer state with sensitive content redacted.
func (r *Reducer) GoString() string { return r.String() }
