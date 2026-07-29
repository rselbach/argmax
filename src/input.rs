//! Incremental, byte-preserving terminal input routing.
//!
//! The router recognizes only the finite keys needed by the wrapper. Every
//! routed event retains the exact input bytes and says whether they must be sent
//! to the shell immediately, only when the wrapper declines the action, or not
//! at all when retained solely for inspection. It performs no terminal I/O.
//!
//! Escape sequences and UTF-8 scalars may span arbitrary read boundaries. The
//! router retains only a bounded possible sequence prefix. Unknown terminal
//! controls remain one desynchronizing decision through their safe boundary;
//! oversized controls are streamed in desynchronizing chunks without exposing
//! their payload as printable input. Bracketed-paste data is streamed back in
//! route events; only a possible end-marker prefix remains between calls.
//!
//! This decoder does not decide whether a foreground program owns the terminal.
//! A controller must ignore wrapper actions and forward every fallback event
//! while lifecycle or synchronization authority is unsafe. It must also clear
//! overlay state synchronously before forwarding Enter and must not request a
//! shell snapshot until a complete input decision has been forwarded.

use std::error::Error;
use std::fmt;

/// Largest configurable terminal sequence accepted by this boundary.
pub const MAX_CONFIGURED_SEQUENCE_BYTES: usize = 32;

/// Largest possible input prefix retained between calls.
pub const MAX_RETAINED_PREFIX_BYTES: usize = MAX_CONFIGURED_SEQUENCE_BYTES;

/// Most new input bytes consumed by one call to [`InputRouter::route`].
pub const MAX_ROUTE_BATCH_INPUT_BYTES: usize = 256;

/// Most exact input bytes represented by one [`RouteBatch`].
///
/// A batch can drain one prefix retained by the preceding call in addition to
/// the new bytes it consumes.
pub const MAX_ROUTE_BATCH_EVENT_BYTES: usize =
    MAX_ROUTE_BATCH_INPUT_BYTES + MAX_RETAINED_PREFIX_BYTES;

/// Most routing decisions returned in one [`RouteBatch`].
///
/// The extra event permits an action-only desynchronization at end of input.
pub const MAX_ROUTE_BATCH_EVENTS: usize = MAX_ROUTE_BATCH_EVENT_BYTES + 1;

const ESCAPE: u8 = 0x1b;
const CANCEL: u8 = 0x18;
const SUBSTITUTE: u8 = 0x1a;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy)]
struct FixedSequence {
    bytes: &'static [u8],
    action: InputAction,
    forwarding: Forwarding,
}

const FIXED_SEQUENCES: [FixedSequence; 19] = [
    FixedSequence {
        bytes: b"\x1b[A",
        action: InputAction::ArrowUp,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1b[B",
        action: InputAction::ArrowDown,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1b[C",
        action: InputAction::ArrowRight,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1b[D",
        action: InputAction::ArrowLeft,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1bOA",
        action: InputAction::ArrowUp,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1bOB",
        action: InputAction::ArrowDown,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1bOC",
        action: InputAction::ArrowRight,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1bOD",
        action: InputAction::ArrowLeft,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: b"\x1b[3~",
        action: InputAction::Delete,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[H",
        action: InputAction::Home,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1bOH",
        action: InputAction::Home,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[1~",
        action: InputAction::Home,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[7~",
        action: InputAction::Home,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[F",
        action: InputAction::End,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1bOF",
        action: InputAction::End,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[4~",
        action: InputAction::End,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[8~",
        action: InputAction::End,
        forwarding: Forwarding::Immediate,
    },
    FixedSequence {
        bytes: b"\x1b[Z",
        action: InputAction::ShiftTab,
        forwarding: Forwarding::OnFallback,
    },
    FixedSequence {
        bytes: BRACKETED_PASTE_START,
        action: InputAction::PasteStart,
        forwarding: Forwarding::Immediate,
    },
];

/// When the exact bytes attached to a route event should reach the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Forwarding {
    /// Forward the bytes unconditionally.
    ///
    /// A controller may first perform a required synchronous state change, such
    /// as clearing the overlay before Enter, but must not suppress these bytes.
    Immediate,
    /// Forward the bytes when the wrapper declines the associated action.
    ///
    /// While a foreground program owns the terminal, or lifecycle or buffer
    /// authority is unsafe, the controller must decline every action and forward
    /// these bytes unchanged.
    OnFallback,
    /// Do not forward these bytes; they are retained only for inspection.
    ///
    /// This is used for the LF half of CRLF after the CR was already forwarded,
    /// and for action-only observations that contain no input bytes.
    Suppress,
}

/// Semantic observation or conditional action associated with exact input bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum InputAction {
    /// One complete printable Unicode scalar was typed.
    Printable(char),
    /// Tab may accept or reveal a suggestion and otherwise falls back to the shell.
    Tab,
    /// Enter executes the shell's authoritative buffer.
    ///
    /// The controller must clear overlay state before forwarding its bytes.
    Enter,
    /// A standalone Escape was resolved by an explicit pending-input flush.
    Escape,
    /// Move upward or enter shell history when wrapper state permits.
    ArrowUp,
    /// Move downward or traverse shell history when wrapper state permits.
    ArrowDown,
    /// Move left unless wrapper state provides a different handled action.
    ArrowLeft,
    /// Move right or accept a displayed ghost suffix when wrapper state permits.
    ArrowRight,
    /// Preserve the shell's backward-deletion behavior.
    Backspace,
    /// Preserve the shell's forward-deletion behavior.
    Delete,
    /// Preserve the shell's beginning-of-line or buffer behavior.
    Home,
    /// Preserve the shell's end-of-line or buffer behavior.
    End,
    /// Preserve the shell's beginning-of-line behavior.
    CtrlA,
    /// Preserve the shell's interrupt behavior and clear stale overlay state.
    CtrlC,
    /// Preserve the shell's end-of-line behavior.
    CtrlE,
    /// Preserve the shell's clear-screen behavior.
    CtrlL,
    /// Preserve the shell's line-clearing behavior.
    CtrlU,
    /// Preserve the shell's word-deletion behavior.
    CtrlW,
    /// Run the configured mode-toggle action.
    ToggleMode,
    /// Run the configured menu-toggle action.
    ToggleMenu,
    /// Shift+Tab was received but was not assigned to a configurable action.
    ShiftTab,
    /// Bracketed paste began.
    PasteStart,
    /// Verbatim bracketed-paste payload bytes were forwarded.
    PasteData,
    /// Bracketed paste ended.
    PasteEnd,
    /// Input could not be classified safely; stale semantic state must be ignored.
    Desynchronize,
}

impl InputAction {
    const fn debug_name(self) -> &'static str {
        match self {
            Self::Printable(_) => "Printable",
            Self::Tab => "Tab",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Home => "Home",
            Self::End => "End",
            Self::CtrlA => "CtrlA",
            Self::CtrlC => "CtrlC",
            Self::CtrlE => "CtrlE",
            Self::CtrlL => "CtrlL",
            Self::CtrlU => "CtrlU",
            Self::CtrlW => "CtrlW",
            Self::ToggleMode => "ToggleMode",
            Self::ToggleMenu => "ToggleMenu",
            Self::ShiftTab => "ShiftTab",
            Self::PasteStart => "PasteStart",
            Self::PasteData => "PasteData",
            Self::PasteEnd => "PasteEnd",
            Self::Desynchronize => "Desynchronize",
        }
    }
}

impl fmt::Debug for InputAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.debug_name())
    }
}

struct RedactedAction(Option<InputAction>);

impl fmt::Debug for RedactedAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(action) => write!(formatter, "Some({})", action.debug_name()),
            None => formatter.write_str("None"),
        }
    }
}

/// One inspectable routing decision for exact terminal input bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct RouteEvent {
    bytes: Vec<u8>,
    forwarding: Forwarding,
    action: Option<InputAction>,
}

impl fmt::Debug for RouteEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteEvent")
            .field("byte_count", &self.bytes.len())
            .field("forwarding", &self.forwarding)
            .field("action", &RedactedAction(self.action))
            .finish()
    }
}

impl RouteEvent {
    fn new(bytes: Vec<u8>, forwarding: Forwarding, action: Option<InputAction>) -> Self {
        Self {
            bytes,
            forwarding,
            action,
        }
    }

    /// Returns the exact bytes represented by this decision.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns when these exact bytes should be forwarded to the shell.
    #[must_use]
    pub const fn forwarding(&self) -> Forwarding {
        self.forwarding
    }

    /// Returns the independent wrapper action or observation, when present.
    #[must_use]
    pub const fn action(&self) -> Option<InputAction> {
        self.action
    }

    /// Splits the event into its exact bytes, forwarding policy, and action.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Forwarding, Option<InputAction>) {
        (self.bytes, self.forwarding, self.action)
    }
}

/// A hard-bounded group of routing decisions from one incremental operation.
///
/// Callers advance their input slice by [`Self::consumed_bytes`] and call
/// [`InputRouter::route`] again until all bytes have been consumed. Each batch
/// contains at most [`MAX_ROUTE_BATCH_EVENTS`] decisions representing at most
/// [`MAX_ROUTE_BATCH_EVENT_BYTES`] exact input bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct RouteBatch {
    consumed_bytes: usize,
    event_bytes: usize,
    events: Vec<RouteEvent>,
}

impl fmt::Debug for RouteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteBatch")
            .field("consumed_bytes", &self.consumed_bytes)
            .field("event_bytes", &self.event_bytes)
            .field("event_count", &self.events.len())
            .field("events", &self.events)
            .finish()
    }
}

impl RouteBatch {
    fn new(consumed_bytes: usize, events: Vec<RouteEvent>) -> Self {
        let event_bytes = events.iter().map(|event| event.bytes.len()).sum();
        debug_assert!(consumed_bytes <= MAX_ROUTE_BATCH_INPUT_BYTES);
        debug_assert!(event_bytes <= MAX_ROUTE_BATCH_EVENT_BYTES);
        debug_assert!(events.len() <= MAX_ROUTE_BATCH_EVENTS);
        Self {
            consumed_bytes,
            event_bytes,
            events,
        }
    }

    /// Returns how many bytes the caller should advance its input slice.
    #[must_use]
    pub const fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    /// Returns the total number of exact input bytes represented by the events.
    #[must_use]
    pub const fn event_bytes(&self) -> usize {
        self.event_bytes
    }

    /// Returns the number of routing decisions in this batch.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// Returns this batch's routing decisions in byte-stream order.
    #[must_use]
    pub fn events(&self) -> &[RouteEvent] {
        &self.events
    }

    /// Takes ownership of this batch's routing decisions.
    #[must_use]
    pub fn into_events(self) -> Vec<RouteEvent> {
        self.events
    }
}

/// Configurable action whose already-resolved terminal bytes were rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfiguredInputAction {
    /// Mode-toggle binding.
    ToggleMode,
    /// Menu-toggle binding.
    ToggleMenu,
}

impl fmt::Display for ConfiguredInputAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ToggleMode => "toggle-mode",
            Self::ToggleMenu => "toggle-menu",
        })
    }
}

/// Invalid resolved input-router configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputRouterError {
    /// A resolved sequence contained no bytes.
    EmptySequence {
        /// Responsible configurable action.
        action: ConfiguredInputAction,
    },
    /// A resolved sequence exceeded the router's retained-prefix bound.
    SequenceTooLong {
        /// Responsible configurable action.
        action: ConfiguredInputAction,
        /// Supplied sequence length in bytes.
        observed_bytes: usize,
        /// Maximum accepted sequence length in bytes.
        limit: usize,
    },
}

impl fmt::Display for InputRouterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySequence { action } => {
                write!(formatter, "{action} terminal sequence is empty")
            }
            Self::SequenceTooLong {
                action,
                observed_bytes,
                limit,
            } => write!(
                formatter,
                "{action} terminal sequence is {observed_bytes} bytes; limit is {limit}"
            ),
        }
    }
}

impl Error for InputRouterError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParameterPhase {
    Parameters,
    Intermediates,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalControlState {
    Parameterized(ParameterPhase),
    IgnoreUntilFinal,
    Escape,
    String {
        allows_bell_terminator: bool,
        escape_pending: bool,
    },
}

impl TerminalControlState {
    fn advance(&mut self, byte: u8) -> ControlAdvance {
        if matches!(byte, CANCEL | SUBSTITUTE) {
            return ControlAdvance::Complete;
        }

        match self {
            Self::Parameterized(phase) => {
                if byte == ESCAPE {
                    return ControlAdvance::RestartEscape;
                }
                match (*phase, byte) {
                    (ParameterPhase::Parameters, 0x20..=0x2f) => {
                        *phase = ParameterPhase::Intermediates;
                        ControlAdvance::Pending
                    }
                    (_, 0x00..=0x1f | 0x7f)
                    | (ParameterPhase::Parameters, 0x30..=0x3f)
                    | (ParameterPhase::Intermediates, 0x20..=0x2f) => ControlAdvance::Pending,
                    (_, 0x40..=0x7e) => ControlAdvance::Complete,
                    _ => {
                        *self = Self::IgnoreUntilFinal;
                        ControlAdvance::Pending
                    }
                }
            }
            Self::IgnoreUntilFinal => {
                if byte == ESCAPE {
                    ControlAdvance::RestartEscape
                } else if matches!(byte, 0x40..=0x7e) {
                    ControlAdvance::Complete
                } else {
                    ControlAdvance::Pending
                }
            }
            Self::Escape => {
                if byte == ESCAPE {
                    return ControlAdvance::RestartEscape;
                }
                if matches!(byte, 0x30..=0x7e) {
                    ControlAdvance::Complete
                } else {
                    ControlAdvance::Pending
                }
            }
            Self::String {
                allows_bell_terminator,
                escape_pending,
            } => {
                if (*allows_bell_terminator && byte == 0x07) || (*escape_pending && byte == b'\\') {
                    return ControlAdvance::Complete;
                }
                *escape_pending = byte == ESCAPE;
                ControlAdvance::Pending
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlAdvance {
    Pending,
    Complete,
    RestartEscape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlPrefixResolution {
    Incomplete(TerminalControlState),
    Complete,
    RestartEscape(usize),
    NotControl,
}

/// Incremental terminal input decoder with bounded cross-chunk state.
pub struct InputRouter {
    toggle_mode: Vec<u8>,
    toggle_menu: Vec<u8>,
    sequence_prefix: Vec<u8>,
    terminal_control_prefix: Vec<u8>,
    terminal_control_state: Option<TerminalControlState>,
    utf8_prefix: Vec<u8>,
    paste_end_prefix: Vec<u8>,
    in_bracketed_paste: bool,
    last_was_carriage_return: bool,
}

impl fmt::Debug for InputRouter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InputRouter")
            .field("toggle_mode_bytes", &self.toggle_mode.len())
            .field("toggle_menu_bytes", &self.toggle_menu.len())
            .field("sequence_prefix_bytes", &self.sequence_prefix.len())
            .field(
                "terminal_control_prefix_bytes",
                &self.terminal_control_prefix.len(),
            )
            .field(
                "terminal_control_pending",
                &self.terminal_control_state.is_some(),
            )
            .field("utf8_prefix_bytes", &self.utf8_prefix.len())
            .field("paste_end_prefix_bytes", &self.paste_end_prefix.len())
            .finish_non_exhaustive()
    }
}

impl InputRouter {
    /// Creates a router from validated, resolved configurable byte sequences.
    ///
    /// Duplicate and prefix-conflicting bindings are expected to have been
    /// rejected by configuration validation. This boundary independently caps
    /// their lengths so malformed callers cannot create unbounded prefix state.
    ///
    /// # Errors
    ///
    /// Returns an error when either sequence is empty or exceeds
    /// [`MAX_CONFIGURED_SEQUENCE_BYTES`].
    pub fn new(toggle_mode: &[u8], toggle_menu: &[u8]) -> Result<Self, InputRouterError> {
        validate_binding(toggle_mode, ConfiguredInputAction::ToggleMode)?;
        validate_binding(toggle_menu, ConfiguredInputAction::ToggleMenu)?;
        Ok(Self {
            toggle_mode: toggle_mode.to_vec(),
            toggle_menu: toggle_menu.to_vec(),
            sequence_prefix: Vec::with_capacity(MAX_CONFIGURED_SEQUENCE_BYTES),
            terminal_control_prefix: Vec::with_capacity(MAX_RETAINED_PREFIX_BYTES),
            terminal_control_state: None,
            utf8_prefix: Vec::with_capacity(4),
            paste_end_prefix: Vec::with_capacity(BRACKETED_PASTE_END.len()),
            in_bracketed_paste: false,
            last_was_carriage_return: false,
        })
    }

    /// Routes at most [`MAX_ROUTE_BATCH_INPUT_BYTES`] bytes from `input`.
    ///
    /// Complete decisions are returned in byte-stream order. A possible Escape,
    /// UTF-8, configurable-binding, or paste-end prefix remains retained until a
    /// later call disambiguates it. Advance the input slice by
    /// [`RouteBatch::consumed_bytes`] before calling again. Call
    /// [`Self::flush_pending`] after the caller's standalone-Escape timeout
    /// expires, and [`Self::finish`] at EOF.
    #[must_use]
    pub fn route(&mut self, input: &[u8]) -> RouteBatch {
        let consumed_bytes = input.len().min(MAX_ROUTE_BATCH_INPUT_BYTES);
        let mut events = Vec::with_capacity(consumed_bytes.saturating_add(1));
        let mut paste_data =
            Vec::with_capacity(consumed_bytes.saturating_add(self.paste_end_prefix.len()));

        for &byte in &input[..consumed_bytes] {
            if self.in_bracketed_paste {
                self.route_paste_byte(byte, &mut paste_data, &mut events);
            } else {
                self.route_normal_byte(byte, &mut events);
            }
        }
        emit_paste_data(&mut paste_data, &mut events);
        RouteBatch::new(consumed_bytes, events)
    }

    /// Resolves input retained through an idle timeout.
    ///
    /// A lone Escape becomes [`InputAction::Escape`]. Every other prefix remains
    /// retained: a timeout is not a defensible boundary for an incomplete CSI,
    /// string control, UTF-8 scalar, or configured binding. While bracketed paste
    /// is active this is likewise a no-op. CRLF coalescing is unaffected.
    #[must_use]
    pub fn flush_pending(&mut self) -> RouteBatch {
        if self.in_bracketed_paste {
            return RouteBatch::new(0, Vec::new());
        }

        let mut events = Vec::new();
        if self.sequence_prefix == [ESCAPE] {
            self.drain_sequence_prefix(&mut events);
        }
        RouteBatch::new(0, events)
    }

    /// Drains retained bytes and resets streaming state at end of input.
    ///
    /// Incomplete terminal-control, Escape, and UTF-8 prefixes are forwarded
    /// intact and desynchronize semantic state. An unfinished bracketed paste
    /// never gains a synthetic end marker: its real retained bytes are forwarded,
    /// or an empty action-only desynchronization is emitted when no bytes remain
    /// buffered.
    #[must_use]
    pub fn finish(&mut self) -> RouteBatch {
        self.last_was_carriage_return = false;
        let was_in_bracketed_paste = self.in_bracketed_paste;
        self.in_bracketed_paste = false;

        let mut events = Vec::new();
        if was_in_bracketed_paste {
            let bytes = std::mem::take(&mut self.paste_end_prefix);
            let forwarding = if bytes.is_empty() {
                Forwarding::Suppress
            } else {
                Forwarding::Immediate
            };
            events.push(RouteEvent::new(
                bytes,
                forwarding,
                Some(InputAction::Desynchronize),
            ));
        } else {
            self.drain_normal_pending(&mut events);
            if self.in_bracketed_paste {
                events.push(RouteEvent::new(
                    Vec::new(),
                    Forwarding::Suppress,
                    Some(InputAction::Desynchronize),
                ));
            }
        }
        self.in_bracketed_paste = false;

        RouteBatch::new(0, events)
    }

    fn drain_normal_pending(&mut self, events: &mut Vec<RouteEvent>) {
        self.drain_terminal_control(events);
        self.drain_sequence_prefix(events);

        if !self.utf8_prefix.is_empty() {
            events.push(desynchronized(std::mem::take(&mut self.utf8_prefix)));
        }
    }

    fn drain_sequence_prefix(&mut self, events: &mut Vec<RouteEvent>) {
        if !self.sequence_prefix.is_empty() {
            let bytes = std::mem::take(&mut self.sequence_prefix);
            if bytes == [ESCAPE] {
                events.push(RouteEvent::new(
                    bytes,
                    Forwarding::Immediate,
                    Some(InputAction::Escape),
                ));
            } else {
                let status = self.sequence_status(&bytes);
                if let Some((action, forwarding)) = status.exact {
                    self.emit_sequence(bytes, action, forwarding, events);
                } else {
                    events.push(desynchronized(bytes));
                }
            }
        }
    }

    fn drain_terminal_control(&mut self, events: &mut Vec<RouteEvent>) {
        if self.terminal_control_state.take().is_none() {
            return;
        }
        let bytes = std::mem::take(&mut self.terminal_control_prefix);
        let forwarding = if bytes.is_empty() {
            Forwarding::Suppress
        } else {
            Forwarding::Immediate
        };
        events.push(RouteEvent::new(
            bytes,
            forwarding,
            Some(InputAction::Desynchronize),
        ));
    }

    /// Returns whether bracketed-paste payload routing is active.
    #[must_use]
    pub const fn is_bracketed_paste(&self) -> bool {
        self.in_bracketed_paste
    }

    /// Returns the number of ambiguous input bytes retained between calls.
    ///
    /// A controller must not inject a shell-synchronization probe while this is
    /// nonzero. Oversized controls are emitted in desynchronizing chunks while at
    /// least one continuing prefix byte remains retained until a real boundary.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.sequence_prefix
            .len()
            .saturating_add(self.terminal_control_prefix.len())
            .saturating_add(self.utf8_prefix.len())
            .saturating_add(self.paste_end_prefix.len())
    }

    fn route_normal_byte(&mut self, byte: u8, events: &mut Vec<RouteEvent>) {
        if self.last_was_carriage_return {
            self.last_was_carriage_return = false;
            if byte == b'\n' {
                events.push(RouteEvent::new(vec![byte], Forwarding::Suppress, None));
                return;
            }
        }

        let initial_event_count = events.len();
        let mut retry = Some(byte);
        while let Some(current) = retry.take() {
            if self.terminal_control_state.is_some() {
                self.route_terminal_control_byte(current, events);
                continue;
            }

            if !self.utf8_prefix.is_empty() {
                if is_utf8_continuation(current) {
                    self.utf8_prefix.push(current);
                    self.finish_utf8_if_complete(events);
                } else {
                    events.push(desynchronized(std::mem::take(&mut self.utf8_prefix)));
                    retry = Some(current);
                }
                continue;
            }

            if !self.sequence_prefix.is_empty() {
                self.sequence_prefix.push(current);
                self.resolve_sequence_prefix(events);
                continue;
            }

            let status = self.sequence_status(&[current]);
            if status.has_match() {
                self.sequence_prefix.push(current);
                self.resolve_sequence_prefix(events);
                continue;
            }

            self.route_single_byte(current, events);
        }

        if byte == b'\r' {
            self.last_was_carriage_return =
                events[initial_event_count..].last().is_some_and(|event| {
                    event.bytes == [b'\r']
                        && event.forwarding == Forwarding::Immediate
                        && event.action == Some(InputAction::Enter)
                });
        }
    }

    fn route_single_byte(&mut self, byte: u8, events: &mut Vec<RouteEvent>) {
        let action = match byte {
            b'\t' => {
                events.push(RouteEvent::new(
                    vec![byte],
                    Forwarding::OnFallback,
                    Some(InputAction::Tab),
                ));
                return;
            }
            b'\r' | b'\n' => InputAction::Enter,
            0x08 | 0x7f => InputAction::Backspace,
            0x01 => InputAction::CtrlA,
            0x03 => InputAction::CtrlC,
            0x05 => InputAction::CtrlE,
            0x0c => InputAction::CtrlL,
            0x15 => InputAction::CtrlU,
            0x17 => InputAction::CtrlW,
            b' '..=b'~' => InputAction::Printable(char::from(byte)),
            0x80..=u8::MAX => {
                if utf8_sequence_length(byte).is_some() {
                    self.utf8_prefix.push(byte);
                    return;
                }
                InputAction::Desynchronize
            }
            _ => InputAction::Desynchronize,
        };
        events.push(RouteEvent::new(
            vec![byte],
            Forwarding::Immediate,
            Some(action),
        ));
    }

    fn finish_utf8_if_complete(&mut self, events: &mut Vec<RouteEvent>) {
        let Some(&first) = self.utf8_prefix.first() else {
            return;
        };
        let Some(expected_length) = utf8_sequence_length(first) else {
            events.push(desynchronized(std::mem::take(&mut self.utf8_prefix)));
            return;
        };
        if self.utf8_prefix.len() < expected_length {
            return;
        }

        let bytes = std::mem::take(&mut self.utf8_prefix);
        let action = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|value| value.chars().next())
            .filter(|character| character.len_utf8() == bytes.len() && !character.is_control())
            .map_or(InputAction::Desynchronize, InputAction::Printable);
        events.push(RouteEvent::new(bytes, Forwarding::Immediate, Some(action)));
    }

    fn route_terminal_control_byte(&mut self, byte: u8, events: &mut Vec<RouteEvent>) {
        if self.terminal_control_prefix.len() == MAX_RETAINED_PREFIX_BYTES {
            events.push(desynchronized(std::mem::take(
                &mut self.terminal_control_prefix,
            )));
        }

        let Some(state) = self.terminal_control_state.as_mut() else {
            events.push(desynchronized(vec![byte]));
            return;
        };
        let advance = state.advance(byte);
        if advance == ControlAdvance::RestartEscape {
            if !self.terminal_control_prefix.is_empty() {
                events.push(desynchronized(std::mem::take(
                    &mut self.terminal_control_prefix,
                )));
            }
            self.terminal_control_state = None;
            self.sequence_prefix.push(byte);
            return;
        }

        self.terminal_control_prefix.push(byte);
        if advance == ControlAdvance::Complete {
            self.terminal_control_state = None;
            events.push(desynchronized(std::mem::take(
                &mut self.terminal_control_prefix,
            )));
        }
    }

    fn resolve_sequence_prefix(&mut self, events: &mut Vec<RouteEvent>) {
        let status = self.sequence_status(&self.sequence_prefix);
        if !status.has_longer {
            if let Some((action, forwarding)) = status.exact {
                let bytes = std::mem::take(&mut self.sequence_prefix);
                self.emit_sequence(bytes, action, forwarding, events);
                return;
            }
        }
        if !status.has_match() {
            let bytes = std::mem::take(&mut self.sequence_prefix);
            self.resolve_unknown_control_prefix(bytes, events);
        }
    }

    fn resolve_unknown_control_prefix(&mut self, mut bytes: Vec<u8>, events: &mut Vec<RouteEvent>) {
        match terminal_control_prefix_resolution(&bytes) {
            ControlPrefixResolution::Incomplete(state) => {
                debug_assert!(bytes.len() <= MAX_RETAINED_PREFIX_BYTES);
                self.terminal_control_prefix = bytes;
                self.terminal_control_state = Some(state);
            }
            ControlPrefixResolution::RestartEscape(index) => {
                let restart = bytes.split_off(index);
                if !bytes.is_empty() {
                    events.push(desynchronized(bytes));
                }
                if restart == [ESCAPE] {
                    self.sequence_prefix = restart;
                } else if !restart.is_empty() {
                    events.push(desynchronized(restart));
                }
            }
            ControlPrefixResolution::Complete | ControlPrefixResolution::NotControl => {
                events.push(desynchronized(bytes));
            }
        }
    }

    fn emit_sequence(
        &mut self,
        bytes: Vec<u8>,
        action: InputAction,
        forwarding: Forwarding,
        events: &mut Vec<RouteEvent>,
    ) {
        if action == InputAction::PasteStart {
            self.in_bracketed_paste = true;
        }
        events.push(RouteEvent::new(bytes, forwarding, Some(action)));
    }

    fn sequence_status(&self, prefix: &[u8]) -> SequenceStatus {
        let mut status = SequenceStatus::default();
        status.consider(
            &self.toggle_mode,
            InputAction::ToggleMode,
            Forwarding::OnFallback,
            prefix,
        );
        status.consider(
            &self.toggle_menu,
            InputAction::ToggleMenu,
            Forwarding::OnFallback,
            prefix,
        );
        for sequence in FIXED_SEQUENCES {
            status.consider(sequence.bytes, sequence.action, sequence.forwarding, prefix);
        }
        status
    }

    fn route_paste_byte(
        &mut self,
        byte: u8,
        paste_data: &mut Vec<u8>,
        events: &mut Vec<RouteEvent>,
    ) {
        self.paste_end_prefix.push(byte);
        while !BRACKETED_PASTE_END.starts_with(&self.paste_end_prefix) {
            paste_data.push(self.paste_end_prefix.remove(0));
        }

        if self.paste_end_prefix == BRACKETED_PASTE_END {
            emit_paste_data(paste_data, events);
            let marker = std::mem::take(&mut self.paste_end_prefix);
            self.in_bracketed_paste = false;
            events.push(RouteEvent::new(
                marker,
                Forwarding::Immediate,
                Some(InputAction::PasteEnd),
            ));
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SequenceStatus {
    exact: Option<(InputAction, Forwarding)>,
    has_longer: bool,
}

impl SequenceStatus {
    fn consider(
        &mut self,
        sequence: &[u8],
        action: InputAction,
        forwarding: Forwarding,
        prefix: &[u8],
    ) {
        if !sequence.starts_with(prefix) {
            return;
        }
        if sequence.len() == prefix.len() {
            if self.exact.is_none() {
                self.exact = Some((action, forwarding));
            }
        } else {
            self.has_longer = true;
        }
    }

    const fn has_match(self) -> bool {
        self.exact.is_some() || self.has_longer
    }
}

fn terminal_control_prefix_resolution(prefix: &[u8]) -> ControlPrefixResolution {
    if prefix.first() != Some(&ESCAPE) || prefix.len() < 2 {
        return ControlPrefixResolution::NotControl;
    }

    let mut state = match prefix[1] {
        b'[' | b'O' => TerminalControlState::Parameterized(ParameterPhase::Parameters),
        b']' => TerminalControlState::String {
            allows_bell_terminator: true,
            escape_pending: false,
        },
        b'P' | b'X' | b'^' | b'_' => TerminalControlState::String {
            allows_bell_terminator: false,
            escape_pending: false,
        },
        0x20..=0x2f => TerminalControlState::Escape,
        ESCAPE => return ControlPrefixResolution::RestartEscape(1),
        CANCEL | SUBSTITUTE | 0x30..=0x7e => return ControlPrefixResolution::Complete,
        _ => TerminalControlState::Escape,
    };

    for (index, &byte) in prefix.iter().enumerate().skip(2) {
        match state.advance(byte) {
            ControlAdvance::Pending => {}
            ControlAdvance::Complete => return ControlPrefixResolution::Complete,
            ControlAdvance::RestartEscape => {
                return ControlPrefixResolution::RestartEscape(index);
            }
        }
    }
    ControlPrefixResolution::Incomplete(state)
}

fn validate_binding(
    sequence: &[u8],
    action: ConfiguredInputAction,
) -> Result<(), InputRouterError> {
    if sequence.is_empty() {
        return Err(InputRouterError::EmptySequence { action });
    }
    if sequence.len() > MAX_CONFIGURED_SEQUENCE_BYTES {
        return Err(InputRouterError::SequenceTooLong {
            action,
            observed_bytes: sequence.len(),
            limit: MAX_CONFIGURED_SEQUENCE_BYTES,
        });
    }
    Ok(())
}

fn emit_paste_data(data: &mut Vec<u8>, events: &mut Vec<RouteEvent>) {
    if data.is_empty() {
        return;
    }
    events.push(RouteEvent::new(
        std::mem::take(data),
        Forwarding::Immediate,
        Some(InputAction::PasteData),
    ));
}

fn desynchronized(bytes: Vec<u8>) -> RouteEvent {
    RouteEvent::new(
        bytes,
        Forwarding::Immediate,
        Some(InputAction::Desynchronize),
    )
}

const fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

const fn utf8_sequence_length(byte: u8) -> Option<usize> {
    match byte {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_SPACE: &[u8] = &[0x00];
    const CTRL_R: &[u8] = &[0x12];

    fn router() -> InputRouter {
        InputRouter::new(CTRL_SPACE, CTRL_R).unwrap()
    }

    fn routed_bytes(events: &[RouteEvent]) -> Vec<u8> {
        events
            .iter()
            .flat_map(|event| event.bytes().iter().copied())
            .collect()
    }

    fn actions(events: &[RouteEvent]) -> Vec<InputAction> {
        events.iter().filter_map(RouteEvent::action).collect()
    }

    fn immediately_forwarded_bytes(events: &[RouteEvent]) -> Vec<u8> {
        events
            .iter()
            .filter(|event| event.forwarding() == Forwarding::Immediate)
            .flat_map(|event| event.bytes().iter().copied())
            .collect()
    }

    fn assert_batch_bounds(batch: &RouteBatch) {
        assert!(batch.consumed_bytes() <= MAX_ROUTE_BATCH_INPUT_BYTES);
        assert!(batch.event_bytes() <= MAX_ROUTE_BATCH_EVENT_BYTES);
        assert!(batch.event_count() <= MAX_ROUTE_BATCH_EVENTS);
        assert_eq!(
            batch.event_bytes(),
            batch
                .events()
                .iter()
                .map(|event| event.bytes().len())
                .sum::<usize>()
        );
    }

    fn route_all(router: &mut InputRouter, input: &[u8]) -> Vec<RouteEvent> {
        let mut offset = 0;
        let mut events = Vec::new();
        while offset < input.len() {
            let batch = router.route(&input[offset..]);
            assert_batch_bounds(&batch);
            assert!(batch.consumed_bytes() > 0);
            offset += batch.consumed_bytes();
            events.extend(batch.into_events());
        }
        events
    }

    fn route_at_every_single_split(input: &[u8]) -> Vec<Vec<RouteEvent>> {
        (0..=input.len())
            .map(|split| {
                let mut router = router();
                let mut events = route_all(&mut router, &input[..split]);
                events.extend(route_all(&mut router, &input[split..]));
                events
            })
            .collect()
    }

    fn route_at_every_partition(input: &[u8]) -> Vec<Vec<RouteEvent>> {
        let boundary_count = input.len().saturating_sub(1);
        assert!(boundary_count < usize::BITS as usize);
        (0..(1_usize << boundary_count))
            .map(|partition| {
                let mut router = router();
                let mut events = Vec::new();
                let mut start = 0;
                for boundary in 1..input.len() {
                    if partition & (1 << (boundary - 1)) != 0 {
                        events.extend(route_all(&mut router, &input[start..boundary]));
                        start = boundary;
                    }
                }
                events.extend(route_all(&mut router, &input[start..]));
                assert_eq!(router.pending_len(), 0);
                events
            })
            .collect()
    }

    #[test]
    fn printable_utf8_is_complete_and_byte_preserving_at_every_split() {
        let input = "Aé界🦀".as_bytes();
        for events in route_at_every_single_split(input) {
            assert_eq!(routed_bytes(&events), input);
            assert_eq!(
                actions(&events),
                [
                    InputAction::Printable('A'),
                    InputAction::Printable('é'),
                    InputAction::Printable('界'),
                    InputAction::Printable('🦀'),
                ]
            );
            assert!(
                events
                    .iter()
                    .all(|event| event.forwarding() == Forwarding::Immediate)
            );
        }
    }

    #[test]
    fn malformed_and_incomplete_utf8_is_forwarded_and_desynchronizes() {
        let mut router = router();
        let mut events = route_all(&mut router, &[0xe2, b'(', 0xa1, 0xf0, 0x9f]);
        assert_eq!(router.pending_len(), 2);
        assert!(router.flush_pending().events().is_empty());
        assert_eq!(router.pending_len(), 2);
        events.extend(router.finish().into_events());

        assert_eq!(routed_bytes(&events), [0xe2, b'(', 0xa1, 0xf0, 0x9f]);
        assert_eq!(
            actions(&events),
            [
                InputAction::Desynchronize,
                InputAction::Printable('('),
                InputAction::Desynchronize,
                InputAction::Desynchronize,
            ]
        );
        assert_eq!(router.pending_len(), 0);
    }

    #[test]
    fn fixed_escape_sequences_decode_at_every_boundary() {
        let cases: &[(&[u8], InputAction, Forwarding)] = &[
            (b"\x1b[A", InputAction::ArrowUp, Forwarding::OnFallback),
            (b"\x1b[B", InputAction::ArrowDown, Forwarding::OnFallback),
            (b"\x1b[C", InputAction::ArrowRight, Forwarding::OnFallback),
            (b"\x1b[D", InputAction::ArrowLeft, Forwarding::OnFallback),
            (b"\x1bOA", InputAction::ArrowUp, Forwarding::OnFallback),
            (b"\x1bOB", InputAction::ArrowDown, Forwarding::OnFallback),
            (b"\x1bOC", InputAction::ArrowRight, Forwarding::OnFallback),
            (b"\x1bOD", InputAction::ArrowLeft, Forwarding::OnFallback),
            (b"\x1b[Z", InputAction::ShiftTab, Forwarding::OnFallback),
            (b"\x1b[3~", InputAction::Delete, Forwarding::Immediate),
            (b"\x1b[H", InputAction::Home, Forwarding::Immediate),
            (b"\x1bOH", InputAction::Home, Forwarding::Immediate),
            (b"\x1b[1~", InputAction::Home, Forwarding::Immediate),
            (b"\x1b[7~", InputAction::Home, Forwarding::Immediate),
            (b"\x1b[F", InputAction::End, Forwarding::Immediate),
            (b"\x1bOF", InputAction::End, Forwarding::Immediate),
            (b"\x1b[4~", InputAction::End, Forwarding::Immediate),
            (b"\x1b[8~", InputAction::End, Forwarding::Immediate),
        ];

        for &(sequence, action, forwarding) in cases {
            for events in route_at_every_single_split(sequence) {
                assert_eq!(routed_bytes(&events), sequence);
                assert_eq!(actions(&events), [action]);
                assert_eq!(events[0].forwarding(), forwarding);
            }
        }
    }

    #[test]
    fn configured_sequences_take_precedence_and_keep_fallback_bytes() {
        let mut router = InputRouter::new(b"\x1b[Z", CTRL_SPACE).unwrap();
        let events = route_all(&mut router, b"\x1b[Z\0");

        assert_eq!(routed_bytes(&events), b"\x1b[Z\0");
        assert_eq!(
            actions(&events),
            [InputAction::ToggleMode, InputAction::ToggleMenu]
        );
        assert!(
            events
                .iter()
                .all(|event| event.forwarding() == Forwarding::OnFallback)
        );
    }

    #[test]
    fn standalone_escape_requires_explicit_flush() {
        let mut router = router();
        assert!(route_all(&mut router, &[ESCAPE]).is_empty());
        assert_eq!(router.pending_len(), 1);

        let events = router.flush_pending().into_events();
        assert_eq!(routed_bytes(&events), [ESCAPE]);
        assert_eq!(actions(&events), [InputAction::Escape]);
        assert_eq!(events[0].forwarding(), Forwarding::Immediate);
        assert_eq!(router.pending_len(), 0);
    }

    #[test]
    fn unknown_and_incomplete_escape_sequences_are_never_swallowed() {
        for input in [b"\x1b[9~".as_slice(), b"\x1b]unknown\x07".as_slice()] {
            for split in 0..=input.len() {
                let mut router = router();
                let mut events = route_all(&mut router, &input[..split]);
                events.extend(route_all(&mut router, &input[split..]));
                events.extend(router.flush_pending().into_events());
                assert_eq!(routed_bytes(&events), input);
                assert!(actions(&events).contains(&InputAction::Desynchronize));
            }
        }

        let mut router = router();
        assert!(route_all(&mut router, b"\x1b[").is_empty());
        assert!(router.flush_pending().events().is_empty());
        assert_eq!(router.pending_len(), 2);
        let events = router.finish().into_events();
        assert_eq!(routed_bytes(&events), b"\x1b[");
        assert_eq!(actions(&events), [InputAction::Desynchronize]);
    }

    #[test]
    fn generic_terminal_controls_are_one_desynchronization_at_every_partition() {
        let cases = [
            b"\x1b[1;5D".as_slice(),
            b"\x1b[?25l".as_slice(),
            b"\x1b[1 q".as_slice(),
            b"\x1b[1 ;5D".as_slice(),
            b"\x1b[1\x013D".as_slice(),
            b"\x1bOP".as_slice(),
            b"\x1b(0".as_slice(),
            b"\x1b]0;Troy\x07".as_slice(),
            b"\x1b]Troy\x1b\\".as_slice(),
            b"\x1bPqTroy\x07\x1b\\".as_slice(),
            b"\x1b^Troy\x1b\\".as_slice(),
            b"\x1b_Troy\x1b\\".as_slice(),
        ];

        for input in cases {
            for events in route_at_every_partition(input) {
                assert_eq!(routed_bytes(&events), input);
                assert_eq!(actions(&events), [InputAction::Desynchronize]);
                assert_eq!(events[0].forwarding(), Forwarding::Immediate);
            }
        }
    }

    #[test]
    fn malformed_escape_payload_is_held_fail_closed_until_eof() {
        let input = b"\x1b\xc3\xa9";
        let mut router = router();
        assert!(route_all(&mut router, input).is_empty());
        assert_eq!(router.pending_len(), input.len());
        assert!(router.flush_pending().events().is_empty());
        assert_eq!(router.pending_len(), input.len());

        let batch = router.finish();
        assert_batch_bounds(&batch);
        assert_eq!(routed_bytes(batch.events()), input);
        assert_eq!(actions(batch.events()), [InputAction::Desynchronize]);
        assert_eq!(router.pending_len(), 0);
    }

    #[test]
    fn timeout_never_turns_partial_terminal_controls_into_safe_boundaries() {
        for input in [
            b"\x1b[1;".as_slice(),
            b"\x1b]Troy".as_slice(),
            b"\x1bPqTroy".as_slice(),
            b"\x1b^Troy".as_slice(),
            b"\x1b_Troy".as_slice(),
        ] {
            let mut router = router();
            assert!(route_all(&mut router, input).is_empty());
            assert_eq!(router.pending_len(), input.len());
            assert!(router.flush_pending().events().is_empty());
            assert_eq!(router.pending_len(), input.len());

            let events = router.finish().into_events();
            assert_eq!(routed_bytes(&events), input);
            assert_eq!(actions(&events), [InputAction::Desynchronize]);
            assert_eq!(router.pending_len(), 0);
        }
    }

    #[test]
    fn generic_control_restart_never_exposes_a_printable_tail() {
        let input = b"\x1b[12\x1b[A";
        for split in 0..=input.len() {
            let mut router = router();
            let mut events = route_all(&mut router, &input[..split]);
            events.extend(route_all(&mut router, &input[split..]));
            assert_eq!(routed_bytes(&events), input);
            assert_eq!(
                actions(&events),
                [InputAction::Desynchronize, InputAction::ArrowUp]
            );
            assert!(
                !actions(&events)
                    .iter()
                    .any(|action| matches!(action, InputAction::Printable(_)))
            );
        }
    }

    #[test]
    fn oversized_and_incomplete_controls_are_bounded_and_fail_closed() {
        let mut complete = b"\x1b]".to_vec();
        complete.extend(std::iter::repeat_n(b'x', MAX_RETAINED_PREFIX_BYTES * 4));
        complete.extend_from_slice(b"\x1b\\");
        let mut complete_router = router();
        let complete_events = route_all(&mut complete_router, &complete);
        assert_eq!(routed_bytes(&complete_events), complete);
        assert!(complete_events.len() > 1);
        assert!(complete_events.iter().all(|event| {
            event.bytes().len() <= MAX_RETAINED_PREFIX_BYTES
                && event.forwarding() == Forwarding::Immediate
                && event.action() == Some(InputAction::Desynchronize)
        }));
        assert_eq!(complete_router.pending_len(), 0);

        let mut incomplete = b"\x1bP".to_vec();
        incomplete.extend(std::iter::repeat_n(b'y', MAX_RETAINED_PREFIX_BYTES * 4));
        let mut incomplete_router = router();
        let mut incomplete_events = route_all(&mut incomplete_router, &incomplete);
        let finish = incomplete_router.finish();
        assert_batch_bounds(&finish);
        incomplete_events.extend(finish.into_events());
        assert_eq!(routed_bytes(&incomplete_events), incomplete);
        assert!(
            incomplete_events
                .iter()
                .all(|event| event.action() == Some(InputAction::Desynchronize))
        );
        assert_eq!(incomplete_router.pending_len(), 0);
        assert!(incomplete_router.finish().events().is_empty());
    }

    #[test]
    fn full_control_prefix_plus_one_batch_hits_but_never_exceeds_event_bound() {
        let mut prefix = b"\x1b]".to_vec();
        prefix.extend(std::iter::repeat_n(
            b'x',
            MAX_RETAINED_PREFIX_BYTES - prefix.len(),
        ));
        let mut router = router();
        let first = router.route(&prefix);
        assert_batch_bounds(&first);
        assert_eq!(first.consumed_bytes(), MAX_RETAINED_PREFIX_BYTES);
        assert_eq!(first.event_bytes(), 0);
        assert_eq!(router.pending_len(), MAX_RETAINED_PREFIX_BYTES);

        let mut input = vec![b'x'; MAX_ROUTE_BATCH_INPUT_BYTES - 1];
        input.push(0x07);
        let batch = router.route(&input);
        assert_batch_bounds(&batch);
        assert_eq!(batch.consumed_bytes(), MAX_ROUTE_BATCH_INPUT_BYTES);
        assert_eq!(batch.event_bytes(), MAX_ROUTE_BATCH_EVENT_BYTES);
        let mut want = prefix;
        want.extend_from_slice(&input);
        assert_eq!(routed_bytes(batch.events()), want);
        assert!(
            batch
                .events()
                .iter()
                .all(|event| event.action() == Some(InputAction::Desynchronize))
        );
        assert_eq!(router.pending_len(), 0);
    }

    #[test]
    fn paste_end_marker_outside_paste_forwards_and_desynchronizes() {
        for events in route_at_every_single_split(BRACKETED_PASTE_END) {
            assert_eq!(routed_bytes(&events), BRACKETED_PASTE_END);
            assert!(actions(&events).contains(&InputAction::Desynchronize));
            assert!(!actions(&events).contains(&InputAction::PasteEnd));
            assert!(
                events
                    .iter()
                    .all(|event| event.forwarding() == Forwarding::Immediate)
            );
        }
    }

    #[test]
    fn tab_has_fallback_bytes_and_enter_only_executes_the_shell_buffer() {
        let mut router = router();
        let events = route_all(&mut router, b"\t\r\n");

        assert_eq!(routed_bytes(&events), b"\t\r\n");
        assert_eq!(events[0].action(), Some(InputAction::Tab));
        assert_eq!(events[0].forwarding(), Forwarding::OnFallback);
        assert_eq!(events[0].bytes(), b"\t");
        assert_eq!(events[1].action(), Some(InputAction::Enter));
        assert_eq!(events[1].forwarding(), Forwarding::Immediate);
        assert_eq!(events[2].action(), None);
        assert_eq!(events[2].forwarding(), Forwarding::Suppress);
        assert_eq!(immediately_forwarded_bytes(&events), b"\r");
        assert_eq!(
            actions(&events)
                .into_iter()
                .filter(|action| *action == InputAction::Enter)
                .count(),
            1
        );
    }

    #[test]
    fn crlf_is_one_enter_across_chunks_and_an_intervening_escape_flush() {
        let mut crlf_router = router();
        let mut events = route_all(&mut crlf_router, b"\r");
        events.extend(route_all(&mut crlf_router, b"\n"));
        assert_eq!(routed_bytes(&events), b"\r\n");
        assert_eq!(actions(&events), [InputAction::Enter]);
        assert_eq!(events[1].forwarding(), Forwarding::Suppress);
        assert_eq!(immediately_forwarded_bytes(&events), b"\r");

        let mut desynchronized_router = router();
        let mut events = route_all(&mut desynchronized_router, b"\x1b[\r\n");
        assert!(events.is_empty());
        events.extend(desynchronized_router.finish().into_events());
        assert_eq!(routed_bytes(&events), b"\x1b[\r\n");
        assert_eq!(actions(&events), [InputAction::Desynchronize]);
        assert_eq!(immediately_forwarded_bytes(&events), b"\x1b[\r\n");

        let mut flushed_router = InputRouter::new(CTRL_SPACE, CTRL_R).unwrap();
        let mut events = route_all(&mut flushed_router, b"\r");
        events.extend(flushed_router.flush_pending().into_events());
        events.extend(route_all(&mut flushed_router, b"\n"));
        assert_eq!(actions(&events), [InputAction::Enter]);
        assert_eq!(events.last().unwrap().forwarding(), Forwarding::Suppress);
        assert_eq!(immediately_forwarded_bytes(&events), b"\r");
    }

    #[test]
    fn fixed_controls_are_forwarded_with_coherent_actions() {
        let input = [0x01, 0x03, 0x05, 0x08, 0x0c, 0x15, 0x17, 0x7f];
        let mut router = router();
        let events = route_all(&mut router, &input);

        assert_eq!(routed_bytes(&events), input);
        assert_eq!(
            actions(&events),
            [
                InputAction::CtrlA,
                InputAction::CtrlC,
                InputAction::CtrlE,
                InputAction::Backspace,
                InputAction::CtrlL,
                InputAction::CtrlU,
                InputAction::CtrlW,
                InputAction::Backspace,
            ]
        );
        assert!(
            events
                .iter()
                .all(|event| event.forwarding() == Forwarding::Immediate)
        );
    }

    #[test]
    fn bracketed_paste_is_verbatim_and_nonsemantic_at_every_split() {
        let payload = b"A\x03\xff\t\r\n\x1b[20xB";
        let mut input = BRACKETED_PASTE_START.to_vec();
        input.extend_from_slice(payload);
        input.extend_from_slice(BRACKETED_PASTE_END);

        for split in 0..=input.len() {
            let mut router = router();
            let mut events = route_all(&mut router, &input[..split]);
            events.extend(route_all(&mut router, &input[split..]));
            assert_eq!(routed_bytes(&events), input);
            let routed_actions = actions(&events);
            assert_eq!(routed_actions.first(), Some(&InputAction::PasteStart));
            assert_eq!(routed_actions.last(), Some(&InputAction::PasteEnd));
            assert!(routed_actions.iter().all(|action| matches!(
                action,
                InputAction::PasteStart | InputAction::PasteData | InputAction::PasteEnd
            )));
            assert!(!router.is_bracketed_paste());
            assert_eq!(router.pending_len(), 0);
        }
    }

    #[test]
    fn paste_markers_decode_when_every_byte_is_a_separate_chunk() {
        let input = b"\x1b[200~Troy\x1b[201~";
        let mut router = router();
        let mut events = Vec::new();
        for byte in input {
            events.extend(route_all(&mut router, &[*byte]));
            assert!(router.pending_len() <= MAX_RETAINED_PREFIX_BYTES);
        }

        assert_eq!(routed_bytes(&events), input);
        assert_eq!(actions(&events).first(), Some(&InputAction::PasteStart));
        assert_eq!(actions(&events).last(), Some(&InputAction::PasteEnd));
    }

    #[test]
    fn paste_payload_is_streamed_while_only_an_end_prefix_is_retained() {
        let mut router = router();
        let start = route_all(&mut router, BRACKETED_PASTE_START);
        assert_eq!(actions(&start), [InputAction::PasteStart]);

        let payload = vec![b'x'; 256 * 1024];
        let events = route_all(&mut router, &payload);
        assert_eq!(routed_bytes(&events), payload);
        assert!(
            actions(&events)
                .iter()
                .all(|action| *action == InputAction::PasteData)
        );
        assert_eq!(router.pending_len(), 0);

        let events = route_all(&mut router, b"tail\x1b");
        assert_eq!(routed_bytes(&events), b"tail");
        assert_eq!(router.pending_len(), 1);
        assert!(router.flush_pending().events().is_empty());
        assert_eq!(router.pending_len(), 1);

        let mut events = route_all(&mut router, b"x");
        events.extend(route_all(&mut router, BRACKETED_PASTE_END));
        assert_eq!(routed_bytes(&events), b"\x1bx\x1b[201~");
        assert_eq!(actions(&events).last(), Some(&InputAction::PasteEnd));
        assert!(!router.is_bracketed_paste());
    }

    #[test]
    fn finish_drains_incomplete_normal_input_and_is_idempotent() {
        let mut escape_router = router();
        assert!(route_all(&mut escape_router, b"\x1b[").is_empty());
        let batch = escape_router.finish();
        assert_batch_bounds(&batch);
        assert_eq!(batch.consumed_bytes(), 0);
        assert_eq!(routed_bytes(batch.events()), b"\x1b[");
        assert_eq!(actions(batch.events()), [InputAction::Desynchronize]);
        assert_eq!(escape_router.pending_len(), 0);
        assert!(escape_router.finish().events().is_empty());

        let mut standalone_escape_router = router();
        assert!(route_all(&mut standalone_escape_router, b"\x1b").is_empty());
        let events = standalone_escape_router.finish().into_events();
        assert_eq!(routed_bytes(&events), b"\x1b");
        assert_eq!(actions(&events), [InputAction::Escape]);

        let mut utf8_router = router();
        assert!(route_all(&mut utf8_router, &[0xf0, 0x9f]).is_empty());
        let events = utf8_router.finish().into_events();
        assert_eq!(routed_bytes(&events), [0xf0, 0x9f]);
        assert_eq!(actions(&events), [InputAction::Desynchronize]);
        assert_eq!(utf8_router.pending_len(), 0);
    }

    #[test]
    fn finish_desynchronizes_truncated_paste_without_synthetic_input() {
        let mut prefixed_router = router();
        let mut prior_events = route_all(&mut prefixed_router, BRACKETED_PASTE_START);
        prior_events.extend(route_all(&mut prefixed_router, b"Greendale\x1b[20"));
        assert!(prefixed_router.is_bracketed_paste());
        assert_eq!(prefixed_router.pending_len(), 4);

        let batch = prefixed_router.finish();
        assert_batch_bounds(&batch);
        assert_eq!(routed_bytes(batch.events()), b"\x1b[20");
        assert_eq!(actions(batch.events()), [InputAction::Desynchronize]);
        assert_eq!(batch.events()[0].forwarding(), Forwarding::Immediate);
        assert!(!prefixed_router.is_bracketed_paste());
        assert_eq!(prefixed_router.pending_len(), 0);
        assert_eq!(
            routed_bytes(&prior_events),
            b"\x1b[200~Greendale".as_slice()
        );

        let mut empty_router = router();
        let start = route_all(&mut empty_router, BRACKETED_PASTE_START);
        assert_eq!(actions(&start), [InputAction::PasteStart]);
        let batch = empty_router.finish();
        assert_eq!(batch.event_bytes(), 0);
        assert_eq!(batch.event_count(), 1);
        assert!(batch.events()[0].bytes().is_empty());
        assert_eq!(batch.events()[0].forwarding(), Forwarding::Suppress);
        assert_eq!(batch.events()[0].action(), Some(InputAction::Desynchronize));
        assert!(!empty_router.is_bracketed_paste());
        assert!(empty_router.finish().events().is_empty());
    }

    #[test]
    fn huge_normal_and_paste_streams_stay_within_each_batch_limit() {
        let normal_input = vec![b'a'; MAX_ROUTE_BATCH_INPUT_BYTES * 4097 + 17];
        let mut normal_router = router();
        let mut offset = 0;
        while offset < normal_input.len() {
            let batch = normal_router.route(&normal_input[offset..]);
            assert_batch_bounds(&batch);
            let want_consumed = (normal_input.len() - offset).min(MAX_ROUTE_BATCH_INPUT_BYTES);
            assert_eq!(batch.consumed_bytes(), want_consumed);
            assert_eq!(
                routed_bytes(batch.events()),
                normal_input[offset..offset + want_consumed]
            );
            assert!(batch.events().iter().all(|event| {
                event.forwarding() == Forwarding::Immediate
                    && event.action() == Some(InputAction::Printable('a'))
            }));
            offset += batch.consumed_bytes();
        }

        let paste_input = vec![b'x'; MAX_ROUTE_BATCH_INPUT_BYTES * 4097 + 17];
        let mut paste_router = router();
        let start = route_all(&mut paste_router, BRACKETED_PASTE_START);
        assert_eq!(actions(&start), [InputAction::PasteStart]);
        let mut offset = 0;
        while offset < paste_input.len() {
            let batch = paste_router.route(&paste_input[offset..]);
            assert_batch_bounds(&batch);
            let want_consumed = (paste_input.len() - offset).min(MAX_ROUTE_BATCH_INPUT_BYTES);
            assert_eq!(batch.consumed_bytes(), want_consumed);
            assert_eq!(
                routed_bytes(batch.events()),
                paste_input[offset..offset + want_consumed]
            );
            assert!(
                batch
                    .events()
                    .iter()
                    .all(|event| event.action() == Some(InputAction::PasteData))
            );
            assert!(paste_router.pending_len() <= MAX_RETAINED_PREFIX_BYTES);
            offset += batch.consumed_bytes();
        }
        let end = route_all(&mut paste_router, BRACKETED_PASTE_END);
        assert_eq!(actions(&end), [InputAction::PasteEnd]);
        assert!(!paste_router.is_bracketed_paste());
    }

    #[test]
    fn debug_diagnostics_redact_input_and_binding_contents() {
        let action_debug = format!("{:?}", InputAction::Printable('T'));
        assert_eq!(action_debug, "Printable");
        assert!(!action_debug.contains('T'));

        let event = RouteEvent::new(
            b"Troy".to_vec(),
            Forwarding::Immediate,
            Some(InputAction::Printable('T')),
        );
        let event_debug = format!("{event:?}");
        assert_eq!(
            event_debug,
            "RouteEvent { byte_count: 4, forwarding: Immediate, action: Some(Printable) }"
        );
        assert!(!event_debug.contains("Troy"));
        assert!(!event_debug.contains("'T'"));

        let batch = RouteBatch::new(4, vec![event]);
        let batch_debug = format!("{batch:?}");
        assert!(batch_debug.contains("event_count: 1"));
        assert!(batch_debug.contains("action: Some(Printable)"));
        assert!(!batch_debug.contains("Troy"));
        assert!(!batch_debug.contains("'T'"));

        let router = InputRouter::new(b"Greendale", b"Community").unwrap();
        let router_debug = format!("{router:?}");
        assert!(router_debug.contains("toggle_mode_bytes: 9"));
        assert!(router_debug.contains("toggle_menu_bytes: 9"));
        assert!(!router_debug.contains("Greendale"));
        assert!(!router_debug.contains("Community"));
    }

    #[test]
    fn router_configuration_and_pending_state_are_hard_bounded() {
        assert_eq!(
            InputRouter::new(&[], CTRL_R).unwrap_err(),
            InputRouterError::EmptySequence {
                action: ConfiguredInputAction::ToggleMode
            }
        );
        let oversized = vec![b'x'; MAX_CONFIGURED_SEQUENCE_BYTES + 1];
        assert_eq!(
            InputRouter::new(CTRL_SPACE, &oversized).unwrap_err(),
            InputRouterError::SequenceTooLong {
                action: ConfiguredInputAction::ToggleMenu,
                observed_bytes: MAX_CONFIGURED_SEQUENCE_BYTES + 1,
                limit: MAX_CONFIGURED_SEQUENCE_BYTES,
            }
        );

        let longest = vec![b'x'; MAX_CONFIGURED_SEQUENCE_BYTES];
        let mut router = InputRouter::new(&longest, CTRL_R).unwrap();
        assert!(route_all(&mut router, &longest[..longest.len() - 1]).is_empty());
        assert_eq!(router.pending_len(), MAX_RETAINED_PREFIX_BYTES - 1);
        assert!(router.flush_pending().events().is_empty());
        assert_eq!(router.pending_len(), MAX_RETAINED_PREFIX_BYTES - 1);
        let events = router.finish().into_events();
        assert_eq!(routed_bytes(&events), &longest[..longest.len() - 1]);
        assert_eq!(actions(&events), [InputAction::Desynchronize]);
        assert_eq!(router.pending_len(), 0);
    }
}
