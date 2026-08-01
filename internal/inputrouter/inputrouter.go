// Package inputrouter incrementally routes byte-preserving terminal input.
//
// The router recognizes only a bounded set of wrapper actions. Unknown terminal
// controls are forwarded as desynchronizing decisions, and ambiguous prefixes
// remain bounded across calls. It performs no terminal I/O.
//
// A controller must decline actions and forward every fallback event while a
// foreground program owns the terminal or lifecycle or synchronization
// authority is unsafe. It must clear overlay state synchronously before
// forwarding Enter and must not request a shell snapshot until a complete input
// decision has been forwarded.
package inputrouter

import (
	"bytes"
	"fmt"
	"unicode"
	"unicode/utf8"
)

const (
	// MaxConfiguredSequenceBytes is the largest configurable terminal sequence.
	MaxConfiguredSequenceBytes = 32

	// MaxRetainedPrefixBytes is the largest possible prefix retained between calls.
	MaxRetainedPrefixBytes = MaxConfiguredSequenceBytes

	// MaxRouteBatchInputBytes is the most new input consumed by one Route call.
	MaxRouteBatchInputBytes = 256

	// MaxRouteBatchEventBytes is the most exact input represented by one batch.
	MaxRouteBatchEventBytes = MaxRouteBatchInputBytes + MaxRetainedPrefixBytes

	// MaxRouteBatchEvents is the most routing decisions returned in one batch.
	MaxRouteBatchEvents = MaxRouteBatchEventBytes + 1

	escape     byte = 0x1b
	cancel     byte = 0x18
	substitute byte = 0x1a
)

var (
	bracketedPasteStart = []byte("\x1b[200~")
	bracketedPasteEnd   = []byte("\x1b[201~")
)

// Forwarding says when an event's exact bytes should reach the shell.
type Forwarding uint8

const (
	// ForwardImmediate unconditionally forwards the bytes.
	ForwardImmediate Forwarding = iota + 1
	// ForwardOnFallback forwards the bytes when the wrapper declines the action.
	// Controllers must decline every action while foreground, lifecycle, or
	// synchronization authority is unsafe.
	ForwardOnFallback
	// ForwardSuppress retains bytes only for inspection and does not forward them.
	ForwardSuppress
)

// String returns the forwarding policy name.
func (f Forwarding) String() string {
	switch f {
	case ForwardImmediate:
		return "Immediate"
	case ForwardOnFallback:
		return "OnFallback"
	case ForwardSuppress:
		return "Suppress"
	default:
		return fmt.Sprintf("Forwarding(%d)", f)
	}
}

// ActionKind identifies a semantic input observation or conditional action.
type ActionKind uint8

const (
	// ActionPrintable identifies one complete printable Unicode scalar.
	ActionPrintable ActionKind = iota + 1
	// ActionTab may accept or reveal a suggestion and otherwise falls back.
	ActionTab
	// ActionEnter executes the shell's authoritative buffer. A controller must
	// clear overlay state before forwarding its bytes.
	ActionEnter
	// ActionEscape identifies a standalone Escape resolved by an explicit flush.
	ActionEscape
	// ActionArrowUp moves upward or enters shell history when permitted.
	ActionArrowUp
	// ActionArrowDown moves downward or traverses shell history when permitted.
	ActionArrowDown
	// ActionArrowLeft moves left unless wrapper state handles it differently.
	ActionArrowLeft
	// ActionArrowRight moves right or accepts a ghost suffix when permitted.
	ActionArrowRight
	// ActionBackspace preserves shell backward deletion.
	ActionBackspace
	// ActionDelete preserves shell forward deletion.
	ActionDelete
	// ActionHome preserves shell beginning-of-line or buffer behavior.
	ActionHome
	// ActionEnd preserves shell end-of-line or buffer behavior.
	ActionEnd
	// ActionCtrlA preserves shell beginning-of-line behavior.
	ActionCtrlA
	// ActionCtrlC preserves shell interrupt behavior.
	ActionCtrlC
	// ActionCtrlE preserves shell end-of-line behavior.
	ActionCtrlE
	// ActionCtrlL preserves shell clear-screen behavior.
	ActionCtrlL
	// ActionCtrlU preserves shell line-clearing behavior.
	ActionCtrlU
	// ActionCtrlW preserves shell word-deletion behavior.
	ActionCtrlW
	// ActionToggleMode runs the configured mode-toggle action.
	ActionToggleMode
	// ActionToggleMenu runs the configured menu-toggle action.
	ActionToggleMenu
	// ActionShiftTab identifies unassigned Shift+Tab input.
	ActionShiftTab
	// ActionPasteStart identifies the bracketed-paste start marker.
	ActionPasteStart
	// ActionPasteData identifies verbatim bracketed-paste payload bytes.
	ActionPasteData
	// ActionPasteEnd identifies the bracketed-paste end marker.
	ActionPasteEnd
	// ActionDesynchronize marks input that could not be classified safely.
	ActionDesynchronize
)

func (k ActionKind) String() string {
	switch k {
	case ActionPrintable:
		return "Printable"
	case ActionTab:
		return "Tab"
	case ActionEnter:
		return "Enter"
	case ActionEscape:
		return "Escape"
	case ActionArrowUp:
		return "ArrowUp"
	case ActionArrowDown:
		return "ArrowDown"
	case ActionArrowLeft:
		return "ArrowLeft"
	case ActionArrowRight:
		return "ArrowRight"
	case ActionBackspace:
		return "Backspace"
	case ActionDelete:
		return "Delete"
	case ActionHome:
		return "Home"
	case ActionEnd:
		return "End"
	case ActionCtrlA:
		return "CtrlA"
	case ActionCtrlC:
		return "CtrlC"
	case ActionCtrlE:
		return "CtrlE"
	case ActionCtrlL:
		return "CtrlL"
	case ActionCtrlU:
		return "CtrlU"
	case ActionCtrlW:
		return "CtrlW"
	case ActionToggleMode:
		return "ToggleMode"
	case ActionToggleMenu:
		return "ToggleMenu"
	case ActionShiftTab:
		return "ShiftTab"
	case ActionPasteStart:
		return "PasteStart"
	case ActionPasteData:
		return "PasteData"
	case ActionPasteEnd:
		return "PasteEnd"
	case ActionDesynchronize:
		return "Desynchronize"
	default:
		return fmt.Sprintf("InputAction(%d)", k)
	}
}

// InputAction is a semantic observation or conditional wrapper action.
type InputAction struct {
	kind      ActionKind
	character rune
}

func action(kind ActionKind) InputAction { return InputAction{kind: kind} }

func printable(character rune) InputAction {
	return InputAction{kind: ActionPrintable, character: character}
}

// Kind returns the action variant.
func (a InputAction) Kind() ActionKind { return a.kind }

// Character returns the printable scalar for ActionPrintable.
func (a InputAction) Character() (rune, bool) {
	return a.character, a.kind == ActionPrintable
}

// String returns a content-redacted action name.
func (a InputAction) String() string { return a.kind.String() }

// GoString returns a content-redacted action name.
func (a InputAction) GoString() string { return a.String() }

// RouteEvent is one inspectable routing decision for exact terminal bytes.
type RouteEvent struct {
	bytes      []byte
	forwarding Forwarding
	action     InputAction
	hasAction  bool
}

func newRouteEvent(
	input []byte,
	forwarding Forwarding,
	action InputAction,
	hasAction bool,
) RouteEvent {
	return RouteEvent{
		bytes:      input,
		forwarding: forwarding,
		action:     action,
		hasAction:  hasAction,
	}
}

// Bytes returns a copy of the exact bytes represented by this decision.
func (e RouteEvent) Bytes() []byte { return bytes.Clone(e.bytes) }

// Forwarding returns when the event's exact bytes should reach the shell.
func (e RouteEvent) Forwarding() Forwarding { return e.forwarding }

// Action returns the independent wrapper action or observation, when present.
func (e RouteEvent) Action() (InputAction, bool) { return e.action, e.hasAction }

// Len returns the number of exact bytes represented by the event.
func (e RouteEvent) Len() int { return len(e.bytes) }

// String returns a structural representation that redacts input content.
func (e RouteEvent) String() string {
	actionName := "None"
	if e.hasAction {
		actionName = fmt.Sprintf("Some(%s)", e.action)
	}
	return fmt.Sprintf(
		"RouteEvent { byte_count: %d, forwarding: %s, action: %s }",
		len(e.bytes), e.forwarding, actionName,
	)
}

// GoString returns a structural representation that redacts input content.
func (e RouteEvent) GoString() string { return e.String() }

// RouteBatch is a hard-bounded group of incremental routing decisions.
type RouteBatch struct {
	consumedBytes int
	eventBytes    int
	events        []RouteEvent
}

func newRouteBatch(consumedBytes int, events []RouteEvent) RouteBatch {
	eventBytes := 0
	for _, event := range events {
		eventBytes += len(event.bytes)
	}
	return RouteBatch{
		consumedBytes: consumedBytes,
		eventBytes:    eventBytes,
		events:        events,
	}
}

// ConsumedBytes returns how far the caller should advance its input slice.
func (b RouteBatch) ConsumedBytes() int { return b.consumedBytes }

// EventBytes returns the total exact input bytes represented by the events.
func (b RouteBatch) EventBytes() int { return b.eventBytes }

// EventCount returns the number of routing decisions in the batch.
func (b RouteBatch) EventCount() int { return len(b.events) }

// Events returns the routing decisions in byte-stream order.
func (b RouteBatch) Events() []RouteEvent { return slicesClone(b.events) }

// String returns a structural representation that redacts input content.
func (b RouteBatch) String() string {
	return fmt.Sprintf(
		"RouteBatch { consumed_bytes: %d, event_bytes: %d, event_count: %d, events: %#v }",
		b.consumedBytes, b.eventBytes, len(b.events), b.events,
	)
}

// GoString returns a structural representation that redacts input content.
func (b RouteBatch) GoString() string { return b.String() }

func slicesClone[S ~[]E, E any](input S) S {
	if input == nil {
		return nil
	}
	return append(S(nil), input...)
}

// ConfiguredInputAction identifies a configurable action with rejected bytes.
type ConfiguredInputAction uint8

const (
	// ConfiguredToggleMode identifies the mode-toggle binding.
	ConfiguredToggleMode ConfiguredInputAction = iota + 1
	// ConfiguredToggleMenu identifies the menu-toggle binding.
	ConfiguredToggleMenu
)

// String returns the configuration name.
func (a ConfiguredInputAction) String() string {
	switch a {
	case ConfiguredToggleMode:
		return "toggle-mode"
	case ConfiguredToggleMenu:
		return "toggle-menu"
	default:
		return fmt.Sprintf("configured-action-%d", a)
	}
}

// ErrorKind classifies invalid resolved input-router configuration.
type ErrorKind uint8

const (
	// ErrorEmptySequence means a resolved sequence contained no bytes.
	ErrorEmptySequence ErrorKind = iota + 1
	// ErrorSequenceTooLong means a sequence exceeded the retained-prefix bound.
	ErrorSequenceTooLong
)

// InputRouterError describes invalid configuration without retaining its bytes.
type InputRouterError struct {
	kind          ErrorKind
	action        ConfiguredInputAction
	observedBytes int
	limit         int
}

// Kind returns the configuration error class.
func (e *InputRouterError) Kind() ErrorKind { return e.kind }

// Action returns the responsible configurable action.
func (e *InputRouterError) Action() ConfiguredInputAction { return e.action }

// ObservedBytes returns the supplied size for ErrorSequenceTooLong.
func (e *InputRouterError) ObservedBytes() int { return e.observedBytes }

// Limit returns the accepted limit for ErrorSequenceTooLong.
func (e *InputRouterError) Limit() int { return e.limit }

// Error returns a content-free configuration error.
func (e *InputRouterError) Error() string {
	if e.kind == ErrorEmptySequence {
		return fmt.Sprintf("%s terminal sequence is empty", e.action)
	}
	return fmt.Sprintf(
		"%s terminal sequence is %d bytes; limit is %d",
		e.action, e.observedBytes, e.limit,
	)
}

// GoString returns a structural, content-free configuration error.
func (e *InputRouterError) GoString() string {
	switch e.kind {
	case ErrorEmptySequence:
		return fmt.Sprintf("EmptySequence { action: %s }", e.action)
	case ErrorSequenceTooLong:
		return fmt.Sprintf(
			"SequenceTooLong { action: %s, observed_bytes: %d, limit: %d }",
			e.action, e.observedBytes, e.limit,
		)
	default:
		return fmt.Sprintf("InputRouterError(%d)", e.kind)
	}
}

type parameterPhase uint8

const (
	phaseParameters parameterPhase = iota
	phaseIntermediates
)

type terminalControlKind uint8

const (
	controlParameterized terminalControlKind = iota
	controlIgnoreUntilFinal
	controlEscape
	controlString
)

type terminalControlState struct {
	kind                 terminalControlKind
	phase                parameterPhase
	allowsBellTerminator bool
	escapePending        bool
}

type controlAdvance uint8

const (
	controlPending controlAdvance = iota
	controlComplete
	controlRestartEscape
)

func (s *terminalControlState) advance(char byte) controlAdvance {
	if char == cancel || char == substitute {
		return controlComplete
	}

	switch s.kind {
	case controlParameterized:
		if char == escape {
			return controlRestartEscape
		}
		switch {
		case s.phase == phaseParameters && char >= 0x20 && char <= 0x2f:
			s.phase = phaseIntermediates
			return controlPending
		case char <= 0x1f || char == 0x7f:
			return controlPending
		case s.phase == phaseParameters && char >= 0x30 && char <= 0x3f:
			return controlPending
		case s.phase == phaseIntermediates && char >= 0x20 && char <= 0x2f:
			return controlPending
		case char >= 0x40 && char <= 0x7e:
			return controlComplete
		default:
			s.kind = controlIgnoreUntilFinal
			return controlPending
		}
	case controlIgnoreUntilFinal:
		switch {
		case char == escape:
			return controlRestartEscape
		case char >= 0x40 && char <= 0x7e:
			return controlComplete
		default:
			return controlPending
		}
	case controlEscape:
		switch {
		case char == escape:
			return controlRestartEscape
		case char >= 0x30 && char <= 0x7e:
			return controlComplete
		default:
			return controlPending
		}
	case controlString:
		if (s.allowsBellTerminator && char == 0x07) ||
			(s.escapePending && char == '\\') {
			return controlComplete
		}
		s.escapePending = char == escape
		return controlPending
	default:
		return controlPending
	}
}

type controlResolutionKind uint8

const (
	resolutionIncomplete controlResolutionKind = iota
	resolutionComplete
	resolutionRestartEscape
	resolutionNotControl
)

type controlPrefixResolution struct {
	kind         controlResolutionKind
	state        terminalControlState
	restartIndex int
}

type fixedSequence struct {
	bytes      []byte
	action     InputAction
	forwarding Forwarding
}

var fixedSequences = []fixedSequence{
	{[]byte("\x1b[A"), action(ActionArrowUp), ForwardOnFallback},
	{[]byte("\x1b[B"), action(ActionArrowDown), ForwardOnFallback},
	{[]byte("\x1b[C"), action(ActionArrowRight), ForwardOnFallback},
	{[]byte("\x1b[D"), action(ActionArrowLeft), ForwardOnFallback},
	{[]byte("\x1bOA"), action(ActionArrowUp), ForwardOnFallback},
	{[]byte("\x1bOB"), action(ActionArrowDown), ForwardOnFallback},
	{[]byte("\x1bOC"), action(ActionArrowRight), ForwardOnFallback},
	{[]byte("\x1bOD"), action(ActionArrowLeft), ForwardOnFallback},
	{[]byte("\x1b[3~"), action(ActionDelete), ForwardImmediate},
	{[]byte("\x1b[H"), action(ActionHome), ForwardImmediate},
	{[]byte("\x1bOH"), action(ActionHome), ForwardImmediate},
	{[]byte("\x1b[1~"), action(ActionHome), ForwardImmediate},
	{[]byte("\x1b[7~"), action(ActionHome), ForwardImmediate},
	{[]byte("\x1b[F"), action(ActionEnd), ForwardImmediate},
	{[]byte("\x1bOF"), action(ActionEnd), ForwardImmediate},
	{[]byte("\x1b[4~"), action(ActionEnd), ForwardImmediate},
	{[]byte("\x1b[8~"), action(ActionEnd), ForwardImmediate},
	{[]byte("\x1b[Z"), action(ActionShiftTab), ForwardOnFallback},
	{bracketedPasteStart, action(ActionPasteStart), ForwardImmediate},
}

// InputRouter is a bounded incremental terminal input decoder.
type InputRouter struct {
	toggleMode            []byte
	toggleMenu            []byte
	sequencePrefix        []byte
	terminalControlPrefix []byte
	terminalControlState  terminalControlState
	hasTerminalControl    bool
	utf8Prefix            []byte
	pasteEndPrefix        []byte
	inBracketedPaste      bool
	lastWasCarriageReturn bool
}

// New creates a router from validated, resolved configurable byte sequences.
func New(toggleMode, toggleMenu []byte) (*InputRouter, error) {
	if err := validateBinding(toggleMode, ConfiguredToggleMode); err != nil {
		return nil, err
	}
	if err := validateBinding(toggleMenu, ConfiguredToggleMenu); err != nil {
		return nil, err
	}
	return &InputRouter{
		toggleMode:            bytes.Clone(toggleMode),
		toggleMenu:            bytes.Clone(toggleMenu),
		sequencePrefix:        make([]byte, 0, MaxConfiguredSequenceBytes),
		terminalControlPrefix: make([]byte, 0, MaxRetainedPrefixBytes),
		utf8Prefix:            make([]byte, 0, utf8.UTFMax),
		pasteEndPrefix:        make([]byte, 0, len(bracketedPasteEnd)),
	}, nil
}

// Reconfigure replaces bindings without discarding retained sequence bytes.
// It returns false without changing bindings while a sequence prefix is pending.
func (r *InputRouter) Reconfigure(toggleMode, toggleMenu []byte) (bool, error) {
	if err := validateBinding(toggleMode, ConfiguredToggleMode); err != nil {
		return false, err
	}
	if err := validateBinding(toggleMenu, ConfiguredToggleMenu); err != nil {
		return false, err
	}
	if len(r.sequencePrefix) != 0 {
		return false, nil
	}
	r.toggleMode = append(r.toggleMode[:0], toggleMode...)
	r.toggleMenu = append(r.toggleMenu[:0], toggleMenu...)
	return true, nil
}

// Route consumes at most MaxRouteBatchInputBytes from input.
func (r *InputRouter) Route(input []byte) RouteBatch {
	consumedBytes := min(len(input), MaxRouteBatchInputBytes)
	events := make([]RouteEvent, 0, consumedBytes+1)
	pasteData := make([]byte, 0, consumedBytes+len(r.pasteEndPrefix))

	for _, char := range input[:consumedBytes] {
		if r.inBracketedPaste {
			r.routePasteByte(char, &pasteData, &events)
		} else {
			r.routeNormalByte(char, &events)
		}
	}
	emitPasteData(&pasteData, &events)
	return newRouteBatch(consumedBytes, events)
}

// FlushPending resolves a standalone Escape after the caller's idle deadline.
// Other incomplete prefixes and bracketed-paste input remain pending.
func (r *InputRouter) FlushPending() RouteBatch {
	if r.inBracketedPaste {
		return newRouteBatch(0, nil)
	}

	var events []RouteEvent
	if bytes.Equal(r.sequencePrefix, []byte{escape}) {
		r.drainSequencePrefix(&events)
	}
	return newRouteBatch(0, events)
}

// Finish drains retained bytes and resets streaming state at end of input.
func (r *InputRouter) Finish() RouteBatch {
	r.lastWasCarriageReturn = false
	wasInBracketedPaste := r.inBracketedPaste
	r.inBracketedPaste = false

	var events []RouteEvent
	if wasInBracketedPaste {
		input := r.takePasteEndPrefix()
		forwarding := ForwardImmediate
		if len(input) == 0 {
			forwarding = ForwardSuppress
		}
		events = append(events, newRouteEvent(
			input, forwarding, action(ActionDesynchronize), true,
		))
	} else {
		r.drainNormalPending(&events)
		if r.inBracketedPaste {
			events = append(events, newRouteEvent(
				nil, ForwardSuppress, action(ActionDesynchronize), true,
			))
		}
	}
	r.inBracketedPaste = false
	return newRouteBatch(0, events)
}

// IsBracketedPaste reports whether paste payload routing is active.
func (r *InputRouter) IsBracketedPaste() bool { return r.inBracketedPaste }

// PendingLen returns ambiguous bytes retained between calls. A controller must
// not inject a shell-synchronization probe while this is nonzero.
func (r *InputRouter) PendingLen() int {
	return len(r.sequencePrefix) + len(r.terminalControlPrefix) +
		len(r.utf8Prefix) + len(r.pasteEndPrefix)
}

// String returns a structural representation that redacts bindings and input.
func (r *InputRouter) String() string {
	return fmt.Sprintf(
		"InputRouter { toggle_mode_bytes: %d, toggle_menu_bytes: %d, sequence_prefix_bytes: %d, terminal_control_prefix_bytes: %d, terminal_control_pending: %t, utf8_prefix_bytes: %d, paste_end_prefix_bytes: %d, .. }",
		len(r.toggleMode), len(r.toggleMenu), len(r.sequencePrefix),
		len(r.terminalControlPrefix), r.hasTerminalControl, len(r.utf8Prefix),
		len(r.pasteEndPrefix),
	)
}

// GoString returns a structural representation that redacts bindings and input.
func (r *InputRouter) GoString() string { return r.String() }

func (r *InputRouter) drainNormalPending(events *[]RouteEvent) {
	r.drainTerminalControl(events)
	r.drainSequencePrefix(events)
	if len(r.utf8Prefix) != 0 {
		*events = append(*events, desynchronized(r.takeUTF8Prefix()))
	}
}

func (r *InputRouter) drainSequencePrefix(events *[]RouteEvent) {
	if len(r.sequencePrefix) == 0 {
		return
	}
	input := r.takeSequencePrefix()
	if bytes.Equal(input, []byte{escape}) {
		*events = append(*events, newRouteEvent(
			input, ForwardImmediate, action(ActionEscape), true,
		))
		return
	}

	status := r.sequenceStatus(input)
	if status.hasExact {
		r.emitSequence(input, status.exactAction, status.exactForwarding, events)
		return
	}
	*events = append(*events, desynchronized(input))
}

func (r *InputRouter) drainTerminalControl(events *[]RouteEvent) {
	if !r.hasTerminalControl {
		return
	}
	r.hasTerminalControl = false
	input := r.takeTerminalControlPrefix()
	forwarding := ForwardImmediate
	if len(input) == 0 {
		forwarding = ForwardSuppress
	}
	*events = append(*events, newRouteEvent(
		input, forwarding, action(ActionDesynchronize), true,
	))
}

func (r *InputRouter) routeNormalByte(char byte, events *[]RouteEvent) {
	if r.lastWasCarriageReturn {
		r.lastWasCarriageReturn = false
		if char == '\n' {
			*events = append(*events, newRouteEvent(
				[]byte{char}, ForwardSuppress, InputAction{}, false,
			))
			return
		}
	}

	initialEventCount := len(*events)
	current := char
	for retry := true; retry; {
		retry = false
		switch {
		case r.hasTerminalControl:
			r.routeTerminalControlByte(current, events)
		case len(r.utf8Prefix) != 0:
			if isUTF8Continuation(current) {
				r.utf8Prefix = append(r.utf8Prefix, current)
				r.finishUTF8IfComplete(events)
			} else {
				*events = append(*events, desynchronized(r.takeUTF8Prefix()))
				retry = true
			}
		case len(r.sequencePrefix) != 0:
			r.sequencePrefix = append(r.sequencePrefix, current)
			r.resolveSequencePrefix(events)
		default:
			status := r.sequenceStatus([]byte{current})
			if status.hasMatch() {
				r.sequencePrefix = append(r.sequencePrefix, current)
				r.resolveSequencePrefix(events)
			} else {
				r.routeSingleByte(current, events)
			}
		}
	}

	if char != '\r' {
		return
	}
	newEvents := (*events)[initialEventCount:]
	if len(newEvents) == 0 {
		return
	}
	last := newEvents[len(newEvents)-1]
	lastAction, hasAction := last.Action()
	r.lastWasCarriageReturn = bytes.Equal(last.bytes, []byte{'\r'}) &&
		last.forwarding == ForwardImmediate && hasAction &&
		lastAction.kind == ActionEnter
}

func (r *InputRouter) routeSingleByte(char byte, events *[]RouteEvent) {
	var inputAction InputAction
	switch char {
	case '\t':
		*events = append(*events, newRouteEvent(
			[]byte{char}, ForwardOnFallback, action(ActionTab), true,
		))
		return
	case '\r', '\n':
		inputAction = action(ActionEnter)
	case 0x08, 0x7f:
		inputAction = action(ActionBackspace)
	case 0x01:
		inputAction = action(ActionCtrlA)
	case 0x03:
		inputAction = action(ActionCtrlC)
	case 0x05:
		inputAction = action(ActionCtrlE)
	case 0x0c:
		inputAction = action(ActionCtrlL)
	case 0x15:
		inputAction = action(ActionCtrlU)
	case 0x17:
		inputAction = action(ActionCtrlW)
	default:
		switch {
		case char >= ' ' && char <= '~':
			inputAction = printable(rune(char))
		case char >= 0x80:
			if utf8SequenceLength(char) != 0 {
				r.utf8Prefix = append(r.utf8Prefix, char)
				return
			}
			inputAction = action(ActionDesynchronize)
		default:
			inputAction = action(ActionDesynchronize)
		}
	}
	*events = append(*events, newRouteEvent(
		[]byte{char}, ForwardImmediate, inputAction, true,
	))
}

func (r *InputRouter) finishUTF8IfComplete(events *[]RouteEvent) {
	if len(r.utf8Prefix) == 0 {
		return
	}
	expectedLength := utf8SequenceLength(r.utf8Prefix[0])
	if expectedLength == 0 {
		*events = append(*events, desynchronized(r.takeUTF8Prefix()))
		return
	}
	if len(r.utf8Prefix) < expectedLength {
		return
	}

	input := r.takeUTF8Prefix()
	character, size := utf8.DecodeRune(input)
	inputAction := action(ActionDesynchronize)
	if (character != utf8.RuneError || size != 1) &&
		size == len(input) && !unicode.IsControl(character) {
		inputAction = printable(character)
	}
	*events = append(*events, newRouteEvent(
		input, ForwardImmediate, inputAction, true,
	))
}

func (r *InputRouter) routeTerminalControlByte(char byte, events *[]RouteEvent) {
	if len(r.terminalControlPrefix) == MaxRetainedPrefixBytes {
		*events = append(*events, desynchronized(r.takeTerminalControlPrefix()))
	}
	if !r.hasTerminalControl {
		*events = append(*events, desynchronized([]byte{char}))
		return
	}

	advance := r.terminalControlState.advance(char)
	if advance == controlRestartEscape {
		if len(r.terminalControlPrefix) != 0 {
			*events = append(*events, desynchronized(r.takeTerminalControlPrefix()))
		}
		r.hasTerminalControl = false
		r.sequencePrefix = append(r.sequencePrefix, char)
		return
	}

	r.terminalControlPrefix = append(r.terminalControlPrefix, char)
	if advance == controlComplete {
		r.hasTerminalControl = false
		*events = append(*events, desynchronized(r.takeTerminalControlPrefix()))
	}
}

func (r *InputRouter) resolveSequencePrefix(events *[]RouteEvent) {
	status := r.sequenceStatus(r.sequencePrefix)
	if !status.hasLonger && status.hasExact {
		input := r.takeSequencePrefix()
		r.emitSequence(input, status.exactAction, status.exactForwarding, events)
		return
	}
	if !status.hasMatch() {
		input := r.takeSequencePrefix()
		r.resolveUnknownControlPrefix(input, events)
	}
}

func (r *InputRouter) resolveUnknownControlPrefix(
	input []byte,
	events *[]RouteEvent,
) {
	resolution := terminalControlPrefixResolution(input)
	switch resolution.kind {
	case resolutionIncomplete:
		r.terminalControlPrefix = append(r.terminalControlPrefix, input...)
		r.terminalControlState = resolution.state
		r.hasTerminalControl = true
	case resolutionRestartEscape:
		restart := bytes.Clone(input[resolution.restartIndex:])
		input = input[:resolution.restartIndex]
		if len(input) != 0 {
			*events = append(*events, desynchronized(input))
		}
		if bytes.Equal(restart, []byte{escape}) {
			r.sequencePrefix = append(r.sequencePrefix, restart...)
		} else if len(restart) != 0 {
			*events = append(*events, desynchronized(restart))
		}
	case resolutionComplete, resolutionNotControl:
		*events = append(*events, desynchronized(input))
	}
}

func (r *InputRouter) emitSequence(
	input []byte,
	inputAction InputAction,
	forwarding Forwarding,
	events *[]RouteEvent,
) {
	if inputAction.kind == ActionPasteStart {
		r.inBracketedPaste = true
	}
	*events = append(*events, newRouteEvent(
		input, forwarding, inputAction, true,
	))
}

type sequenceStatus struct {
	exactAction     InputAction
	exactForwarding Forwarding
	hasExact        bool
	hasLonger       bool
}

func (s *sequenceStatus) consider(
	sequence []byte,
	inputAction InputAction,
	forwarding Forwarding,
	prefix []byte,
) {
	if !bytes.HasPrefix(sequence, prefix) {
		return
	}
	if len(sequence) == len(prefix) {
		if !s.hasExact {
			s.exactAction = inputAction
			s.exactForwarding = forwarding
			s.hasExact = true
		}
		return
	}
	s.hasLonger = true
}

func (s sequenceStatus) hasMatch() bool { return s.hasExact || s.hasLonger }

func (r *InputRouter) sequenceStatus(prefix []byte) sequenceStatus {
	var status sequenceStatus
	status.consider(
		r.toggleMode, action(ActionToggleMode), ForwardOnFallback, prefix,
	)
	status.consider(
		r.toggleMenu, action(ActionToggleMenu), ForwardOnFallback, prefix,
	)
	for _, sequence := range fixedSequences {
		status.consider(
			sequence.bytes, sequence.action, sequence.forwarding, prefix,
		)
	}
	return status
}

func (r *InputRouter) routePasteByte(
	char byte,
	pasteData *[]byte,
	events *[]RouteEvent,
) {
	r.pasteEndPrefix = append(r.pasteEndPrefix, char)
	for !bytes.HasPrefix(bracketedPasteEnd, r.pasteEndPrefix) {
		*pasteData = append(*pasteData, r.pasteEndPrefix[0])
		copy(r.pasteEndPrefix, r.pasteEndPrefix[1:])
		r.pasteEndPrefix = r.pasteEndPrefix[:len(r.pasteEndPrefix)-1]
	}

	if bytes.Equal(r.pasteEndPrefix, bracketedPasteEnd) {
		emitPasteData(pasteData, events)
		marker := r.takePasteEndPrefix()
		r.inBracketedPaste = false
		*events = append(*events, newRouteEvent(
			marker, ForwardImmediate, action(ActionPasteEnd), true,
		))
	}
}

func terminalControlPrefixResolution(prefix []byte) controlPrefixResolution {
	if len(prefix) < 2 || prefix[0] != escape {
		return controlPrefixResolution{kind: resolutionNotControl}
	}

	var state terminalControlState
	switch second := prefix[1]; {
	case second == '[' || second == 'O':
		state = terminalControlState{
			kind:  controlParameterized,
			phase: phaseParameters,
		}
	case second == ']':
		state = terminalControlState{
			kind:                 controlString,
			allowsBellTerminator: true,
		}
	case second == 'P' || second == 'X' || second == '^' || second == '_':
		state = terminalControlState{kind: controlString}
	case second >= 0x20 && second <= 0x2f:
		state = terminalControlState{kind: controlEscape}
	case second == escape:
		return controlPrefixResolution{
			kind: resolutionRestartEscape, restartIndex: 1,
		}
	case second == cancel || second == substitute ||
		(second >= 0x30 && second <= 0x7e):
		return controlPrefixResolution{kind: resolutionComplete}
	default:
		state = terminalControlState{kind: controlEscape}
	}

	for index := 2; index < len(prefix); index++ {
		switch state.advance(prefix[index]) {
		case controlComplete:
			return controlPrefixResolution{kind: resolutionComplete}
		case controlRestartEscape:
			return controlPrefixResolution{
				kind: resolutionRestartEscape, restartIndex: index,
			}
		case controlPending:
		}
	}
	return controlPrefixResolution{kind: resolutionIncomplete, state: state}
}

func validateBinding(
	sequence []byte,
	configuredAction ConfiguredInputAction,
) error {
	if len(sequence) == 0 {
		return &InputRouterError{
			kind: ErrorEmptySequence, action: configuredAction,
		}
	}
	if len(sequence) > MaxConfiguredSequenceBytes {
		return &InputRouterError{
			kind:          ErrorSequenceTooLong,
			action:        configuredAction,
			observedBytes: len(sequence),
			limit:         MaxConfiguredSequenceBytes,
		}
	}
	return nil
}

func emitPasteData(data *[]byte, events *[]RouteEvent) {
	if len(*data) == 0 {
		return
	}
	input := *data
	*data = nil
	*events = append(*events, newRouteEvent(
		input, ForwardImmediate, action(ActionPasteData), true,
	))
}

func desynchronized(input []byte) RouteEvent {
	return newRouteEvent(
		input, ForwardImmediate, action(ActionDesynchronize), true,
	)
}

func isUTF8Continuation(char byte) bool { return char&0xc0 == 0x80 }

func utf8SequenceLength(char byte) int {
	switch {
	case char >= 0xc2 && char <= 0xdf:
		return 2
	case char >= 0xe0 && char <= 0xef:
		return 3
	case char >= 0xf0 && char <= 0xf4:
		return 4
	default:
		return 0
	}
}

func (r *InputRouter) takeSequencePrefix() []byte {
	input := r.sequencePrefix
	r.sequencePrefix = make([]byte, 0, MaxConfiguredSequenceBytes)
	return input
}

func (r *InputRouter) takeTerminalControlPrefix() []byte {
	input := r.terminalControlPrefix
	r.terminalControlPrefix = make([]byte, 0, MaxRetainedPrefixBytes)
	return input
}

func (r *InputRouter) takeUTF8Prefix() []byte {
	input := r.utf8Prefix
	r.utf8Prefix = make([]byte, 0, utf8.UTFMax)
	return input
}

func (r *InputRouter) takePasteEndPrefix() []byte {
	input := r.pasteEndPrefix
	r.pasteEndPrefix = make([]byte, 0, len(bracketedPasteEnd))
	return input
}
