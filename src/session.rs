//! Deterministic session actions between terminal input and shell authority.
//!
//! This reducer performs no I/O. It forwards exact routed input through inert
//! effects, owns completion-query authority, and treats accepted shell events as
//! the only source of editable-buffer truth. Buffer replacements are requests:
//! they never become authoritative until a later shell snapshot confirms them.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::completion::{CompletionQuery, ProviderBatch, Suggestion, SuggestionSource, TextEdit};
use crate::coordinator::{
    AuthorityRejection, BatchOutcome, CompletionCoordinator, MAX_QUERY_CWD_BYTES,
    MAX_QUERY_LINE_BYTES, PresentationOutcome, QueryStartError, QueryWork, RegistrationError,
};
use crate::input::{
    Forwarding, InputAction, InputRouter, InputRouterError, MAX_ROUTE_BATCH_EVENT_BYTES,
    MAX_ROUTE_BATCH_EVENTS, RouteBatch,
};
use crate::selection::{SelectionState, ghost_suffix};
use crate::shell_events::{
    BufferSyncCapability, DecodedFrame, ForegroundCommandState, InputGenerationError,
    ProbeRequestError, ShellEvent, ShellSessionState, SnapshotNonce, StateUpdate, StreamEpoch,
};

/// Most effects one bounded input reduction can emit.
///
/// Input routing already caps decisions. Seven effects per decision leaves
/// room for boundary release, invalidation, mode, replacement with its paired
/// synchronization request, rendering, and forwarding, followed by one
/// batch-final synchronization request.
pub const MAX_SESSION_EFFECTS: usize = MAX_ROUTE_BATCH_EVENTS * 7 + 1;

/// Completion mode selected for the current session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMode {
    /// Complete from command specifications and related local sources.
    Spec,
    /// Search history, with providers free to include useful spec fallbacks.
    History,
}

/// A validated full replacement for the shell-native editing buffer.
#[derive(Clone, Eq, PartialEq)]
pub struct BufferReplacement {
    text: Box<str>,
    cursor: usize,
}

impl fmt::Debug for BufferReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferReplacement")
            .field("byte_count", &self.text.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl BufferReplacement {
    /// Validates and compacts an exact UTF-8 shell-buffer replacement.
    ///
    /// # Errors
    ///
    /// Returns an error when the text exceeds the authoritative-buffer bound or
    /// the cursor is not a UTF-8 byte boundary in it.
    pub fn new(text: impl Into<String>, cursor: usize) -> Result<Self, ReplacementError> {
        let text = text.into();
        if text.len() > MAX_QUERY_LINE_BYTES {
            return Err(ReplacementError::TooLarge {
                bytes: text.len(),
                limit: MAX_QUERY_LINE_BYTES,
            });
        }
        if !text.is_char_boundary(cursor) {
            return Err(ReplacementError::InvalidCursor {
                cursor,
                line_bytes: text.len(),
            });
        }
        Ok(Self {
            text: text.into_boxed_str(),
            cursor,
        })
    }

    /// Returns the exact UTF-8 bytes requested for the shell buffer.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Returns the exact requested shell-buffer text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Returns the requested UTF-8 byte cursor.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the requested buffer size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Returns whether the requested buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Invalid shell-buffer replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplacementError {
    /// Replacement exceeded the authoritative-buffer bound.
    TooLarge {
        /// Observed UTF-8 byte count.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Cursor was outside the text or split a UTF-8 scalar.
    InvalidCursor {
        /// Supplied UTF-8 byte cursor.
        cursor: usize,
        /// Replacement length in bytes.
        line_bytes: usize,
    },
}

impl fmt::Display for ReplacementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "shell replacement is {bytes} bytes; limit is {limit}"
                )
            }
            Self::InvalidCursor { cursor, line_bytes } => write!(
                formatter,
                "shell replacement cursor {cursor} is invalid for {line_bytes} bytes"
            ),
        }
    }
}

impl Error for ReplacementError {}

/// Non-I/O work requested by one reducer transition.
pub enum SessionEffect {
    /// Send these bytes to the wrapped shell exactly once.
    ForwardInput(Box<[u8]>),
    /// Clear every menu and ghost cell before any following terminal effect.
    ClearOverlay,
    /// Redraw menu and ghost text from [`SessionReducer::selection`].
    RefreshOverlay,
    /// Ask the shell-native editor to replace its real buffer without execution.
    ReplaceBuffer(BufferReplacement),
    /// Inject the reserved synchronization sequence for this correlated nonce.
    RequestBufferSync(SnapshotNonce),
    /// Dispatch bounded provider work without delaying input forwarding.
    StartQuery {
        /// Mode that selected the provider set for this query.
        mode: SessionMode,
        /// Whether this query followed an exact, non-pasted typed ASCII space.
        ///
        /// Providers still validate alias syntax and configuration off the
        /// input path before proposing any edit.
        alias_expansion: bool,
        /// Immutable query and observer-only cancellation handle.
        work: QueryWork,
    },
    /// Redraw mode-dependent UI state.
    ModeChanged(SessionMode),
    /// Surface a closed failure without panicking or performing I/O.
    Fault(SessionFault),
}

impl fmt::Debug for SessionEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForwardInput(bytes) => formatter
                .debug_struct("ForwardInput")
                .field("byte_count", &bytes.len())
                .finish(),
            Self::ClearOverlay => formatter.write_str("ClearOverlay"),
            Self::RefreshOverlay => formatter.write_str("RefreshOverlay"),
            Self::ReplaceBuffer(replacement) => formatter
                .debug_tuple("ReplaceBuffer")
                .field(replacement)
                .finish(),
            Self::RequestBufferSync(nonce) => formatter
                .debug_tuple("RequestBufferSync")
                .field(nonce)
                .finish(),
            Self::StartQuery {
                mode,
                alias_expansion,
                work,
            } => formatter
                .debug_struct("StartQuery")
                .field("mode", mode)
                .field("alias_expansion", alias_expansion)
                .field("work", work)
                .finish(),
            Self::ModeChanged(mode) => formatter.debug_tuple("ModeChanged").field(mode).finish(),
            Self::Fault(fault) => formatter.debug_tuple("Fault").field(fault).finish(),
        }
    }
}

/// Closed failure surfaced by the pure controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFault {
    /// Local-input generations were exhausted rather than wrapped.
    InputGenerationExhausted,
    /// Queued input boundaries were exhausted rather than wrapped.
    InputBoundaryExhausted,
    /// A bounded authoritative query could not be started.
    QueryStart(QueryStartError),
    /// A required synchronization probe could not be reserved.
    Probe(ProbeRequestError),
}

/// Bounded ordered effects from one reducer operation.
#[derive(Default)]
pub struct EffectBatch {
    effects: Vec<SessionEffect>,
}

impl fmt::Debug for EffectBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectBatch")
            .field("effect_count", &self.effects.len())
            .field("effects", &self.effects)
            .finish()
    }
}

impl EffectBatch {
    /// Returns ordered effects.
    #[must_use]
    pub fn effects(&self) -> &[SessionEffect] {
        &self.effects
    }

    /// Takes ownership of ordered effects.
    #[must_use]
    pub fn into_effects(self) -> Vec<SessionEffect> {
        self.effects
    }

    /// Returns the number of effects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// Returns whether no effects were requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    fn push(&mut self, effect: SessionEffect) {
        debug_assert!(self.effects.len() < MAX_SESSION_EFFECTS);
        self.effects.push(effect);
    }

    fn forward(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        match self.effects.last_mut() {
            Some(SessionEffect::ForwardInput(previous))
                if previous.len().saturating_add(bytes.len()) <= MAX_ROUTE_BATCH_EVENT_BYTES =>
            {
                let mut combined = Vec::with_capacity(previous.len() + bytes.len());
                combined.extend_from_slice(previous);
                combined.extend_from_slice(&bytes);
                *previous = combined.into_boxed_slice();
                return;
            }
            _ => {}
        }
        self.push(SessionEffect::ForwardInput(bytes.into_boxed_slice()));
    }
}

/// Input consumption paired with ordered inert effects.
#[derive(Debug)]
pub struct InputReduction {
    consumed_bytes: usize,
    effects: EffectBatch,
}

impl InputReduction {
    /// Returns how many caller bytes were consumed.
    #[must_use]
    pub const fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    /// Returns ordered effects.
    #[must_use]
    pub const fn effects(&self) -> &EffectBatch {
        &self.effects
    }

    /// Takes ownership of the consumption count and effects.
    #[must_use]
    pub fn into_parts(self) -> (usize, EffectBatch) {
        (self.consumed_bytes, self.effects)
    }
}

/// Ranked-presentation result paired with any required UI or buffer effects.
#[derive(Debug)]
pub struct PresentationReduction {
    outcome: PresentationOutcome,
    effects: EffectBatch,
}

impl PresentationReduction {
    /// Returns the coordinator disposition.
    pub const fn outcome(&self) -> &PresentationOutcome {
        &self.outcome
    }

    /// Returns ordered effects.
    #[must_use]
    pub const fn effects(&self) -> &EffectBatch {
        &self.effects
    }

    /// Takes ownership of the disposition and effects.
    pub fn into_parts(self) -> (PresentationOutcome, EffectBatch) {
        (self.outcome, self.effects)
    }
}

/// Invalid reducer construction.
#[derive(Debug)]
pub enum SessionBuildError {
    /// Input bindings could not be routed safely.
    Input(InputRouterError),
    /// Completion providers or UI bound were invalid.
    Completion(RegistrationError),
    /// Working directory exceeded the provider-query path bound.
    CwdTooLarge {
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Working directory was empty or relative.
    CwdNotAbsolute,
}

impl fmt::Display for SessionBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(formatter, "invalid input bindings: {error}"),
            Self::Completion(error) => write!(formatter, "invalid completion setup: {error}"),
            Self::CwdTooLarge { bytes, limit } => {
                write!(formatter, "session cwd is {bytes} bytes; limit is {limit}")
            }
            Self::CwdNotAbsolute => formatter.write_str("session cwd must be absolute"),
        }
    }
}

impl Error for SessionBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Input(error) => Some(error),
            Self::Completion(error) => Some(error),
            Self::CwdTooLarge { .. } | Self::CwdNotAbsolute => None,
        }
    }
}

impl From<InputRouterError> for SessionBuildError {
    fn from(error: InputRouterError) -> Self {
        Self::Input(error)
    }
}

impl From<RegistrationError> for SessionBuildError {
    fn from(error: RegistrationError) -> Self {
        Self::Completion(error)
    }
}

/// Invalid working-directory update rejected without changing session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CwdUpdateError {
    /// Working directory exceeded the provider-query path bound.
    TooLarge {
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// Working directory was empty or relative.
    NotAbsolute,
}

impl fmt::Display for CwdUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, limit } => {
                write!(formatter, "session cwd is {bytes} bytes; limit is {limit}")
            }
            Self::NotAbsolute => formatter.write_str("session cwd must be absolute"),
        }
    }
}

impl Error for CwdUpdateError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplacementKind {
    Acceptance,
    AliasExpansion,
    Ghost,
    HistoryPreview,
    HistoryRestore,
}

#[derive(Clone, Eq, PartialEq)]
struct PendingReplacement {
    replacement: BufferReplacement,
    kind: ReplacementKind,
}

impl fmt::Debug for PendingReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingReplacement")
            .field("replacement", &self.replacement)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Pure controller for one wrapped interactive shell session.
// These flags track independent input, preview, and probe facts.
#[allow(clippy::struct_excessive_bools)]
pub struct SessionReducer {
    input: InputRouter,
    shell: ShellSessionState,
    completion: CompletionCoordinator,
    cwd: PathBuf,
    mode: SessionMode,
    pending_replacement: Option<PendingReplacement>,
    history_origin: Option<BufferReplacement>,
    history_preview_active: bool,
    recall_history_when_ready: bool,
    alias_expansion_pending: bool,
    paste_active: bool,
    probe_needed: bool,
    input_boundary_fence: bool,
    queued_input_boundaries: u64,
    fenced_input_pending: bool,
    fence_prompt_observed: bool,
    fence_sync_nonce: Option<SnapshotNonce>,
    query_restart_deferred: bool,
}

impl fmt::Debug for SessionReducer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionReducer")
            .field("input", &self.input)
            .field("shell", &self.shell)
            .field("completion", &self.completion)
            .field("cwd_bytes", &self.cwd.as_os_str().as_encoded_bytes().len())
            .field("mode", &self.mode)
            .field("pending_replacement", &self.pending_replacement)
            .field("history_origin", &self.history_origin)
            .field("history_preview_active", &self.history_preview_active)
            .field("recall_history_when_ready", &self.recall_history_when_ready)
            .field("alias_expansion_pending", &self.alias_expansion_pending)
            .field("paste_active", &self.paste_active)
            .field("probe_needed", &self.probe_needed)
            .field("input_boundary_fence", &self.input_boundary_fence)
            .field("queued_input_boundaries", &self.queued_input_boundaries)
            .field("fenced_input_pending", &self.fenced_input_pending)
            .field("fence_prompt_observed", &self.fence_prompt_observed)
            .field("fence_sync_nonce", &self.fence_sync_nonce)
            .field("query_restart_deferred", &self.query_restart_deferred)
            .finish()
    }
}

impl SessionReducer {
    /// Creates a bounded reducer for one shell-event stream.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input bindings, provider registration, UI
    /// result bounds, or working-directory bounds.
    pub fn new(
        epoch: StreamEpoch,
        toggle_mode: &[u8],
        toggle_menu: &[u8],
        providers: impl IntoIterator<Item = &'static str>,
        ui_max_suggestions: usize,
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, SessionBuildError> {
        Self::new_with_mode(
            epoch,
            toggle_mode,
            toggle_menu,
            providers,
            ui_max_suggestions,
            cwd,
            SessionMode::Spec,
        )
    }

    /// Creates a bounded reducer with an explicitly resolved initial mode.
    ///
    /// This is the startup boundary for fixed-mode configuration and persisted
    /// `last` mode. Later changes still flow through normal input actions.
    ///
    /// # Errors
    ///
    /// Returns the same validation failures as [`Self::new`].
    pub fn new_with_mode(
        epoch: StreamEpoch,
        toggle_mode: &[u8],
        toggle_menu: &[u8],
        providers: impl IntoIterator<Item = &'static str>,
        ui_max_suggestions: usize,
        cwd: impl Into<PathBuf>,
        initial_mode: SessionMode,
    ) -> Result<Self, SessionBuildError> {
        let cwd = validate_cwd(cwd.into()).map_err(|error| match error {
            CwdUpdateError::TooLarge { bytes, limit } => {
                SessionBuildError::CwdTooLarge { bytes, limit }
            }
            CwdUpdateError::NotAbsolute => SessionBuildError::CwdNotAbsolute,
        })?;

        Ok(Self {
            input: InputRouter::new(toggle_mode, toggle_menu)?,
            shell: ShellSessionState::new(epoch),
            completion: CompletionCoordinator::new(providers, ui_max_suggestions)?,
            cwd,
            mode: initial_mode,
            pending_replacement: None,
            history_origin: None,
            history_preview_active: false,
            recall_history_when_ready: false,
            alias_expansion_pending: false,
            paste_active: false,
            probe_needed: false,
            input_boundary_fence: false,
            queued_input_boundaries: 0,
            fenced_input_pending: false,
            fence_prompt_observed: false,
            fence_sync_nonce: None,
            query_restart_deferred: false,
        })
    }

    /// Returns the current completion mode.
    #[must_use]
    pub const fn mode(&self) -> SessionMode {
        self.mode
    }

    /// Returns correlated shell lifecycle and editable-buffer authority.
    #[must_use]
    pub const fn shell(&self) -> &ShellSessionState {
        &self.shell
    }

    /// Returns the one selection shared by menu and ghost rendering.
    #[must_use]
    pub const fn selection(&self) -> &SelectionState {
        self.completion.selection()
    }

    /// Returns the active immutable provider query, if any.
    #[must_use]
    pub fn active_query(&self) -> Option<&CompletionQuery> {
        self.completion.active_query()
    }

    /// Returns the bounded absolute working directory used for new queries.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Atomically changes the working directory used by provider queries.
    ///
    /// Current provider work is cancelled after validation. A new query starts
    /// immediately only when shell buffer authority is safe; otherwise the next
    /// accepted shell snapshot uses the new directory.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the directory, query, selection, or
    /// cancellation state when `cwd` is relative or exceeds the hard path bound.
    pub fn update_cwd(&mut self, cwd: impl Into<PathBuf>) -> Result<EffectBatch, CwdUpdateError> {
        let cwd = validate_cwd(cwd.into())?;
        self.cwd = cwd;

        let mut effects = EffectBatch::default();
        self.clear_completion(&mut effects);
        if self.actions_are_safe() {
            self.start_query_from_authority(&mut effects);
        } else {
            self.query_restart_deferred = true;
        }
        Ok(effects)
    }

    /// Applies live keybinding and result-limit settings without replacing the
    /// shell, stream epoch, authoritative buffer, or current directory.
    ///
    /// A retained partial configurable key sequence defers the whole change so
    /// already received input keeps its original interpretation. `Ok(None)`
    /// asks the event loop to retry after that prefix resolves.
    ///
    /// # Errors
    ///
    /// Returns a construction-style validation failure without discarding
    /// shell authority.
    pub fn reconfigure(
        &mut self,
        toggle_mode: &[u8],
        toggle_menu: &[u8],
        ui_max_suggestions: usize,
    ) -> Result<Option<EffectBatch>, SessionBuildError> {
        if !self.input.reconfigure(toggle_mode, toggle_menu)? {
            return Ok(None);
        }
        self.completion.reconfigure_ui_limit(ui_max_suggestions)?;
        let mut effects = EffectBatch::default();
        self.clear_completion(&mut effects);
        if self.actions_are_safe() {
            self.start_query_from_authority(&mut effects);
        } else {
            self.query_restart_deferred = true;
        }
        Ok(Some(effects))
    }

    /// Returns whether accepted context is waiting for safe buffer authority.
    ///
    /// The reducer consumes this restart after paste ends or a later shell
    /// snapshot makes query creation safe.
    #[must_use]
    pub const fn query_restart_deferred(&self) -> bool {
        self.query_restart_deferred
    }

    /// Returns whether a shell-native replacement awaits authoritative evidence.
    #[must_use]
    pub const fn replacement_pending(&self) -> bool {
        self.pending_replacement.is_some()
    }

    /// Routes a bounded input prefix and reduces it without provider or PTY I/O.
    #[must_use]
    pub fn route_input(&mut self, input: &[u8]) -> InputReduction {
        let batch = self.input.route(input);
        self.reduce_route_batch(batch)
    }

    /// Resolves a pending standalone Escape after the caller's timeout.
    #[must_use]
    pub fn flush_input(&mut self) -> InputReduction {
        let batch = self.input.flush_pending();
        self.reduce_route_batch(batch)
    }

    /// Drains incomplete input without inventing bytes.
    ///
    /// Used at EOF and when interception ends while the router retains a
    /// partial sequence, so the retained bytes precede any directly
    /// forwarded input.
    #[must_use]
    pub fn finish_input(&mut self) -> InputReduction {
        let batch = self.input.finish();
        self.reduce_route_batch(batch)
    }

    /// Applies one ordered decoded shell frame and derives controller effects.
    #[must_use]
    pub fn apply_shell_frame(&mut self, frame: DecodedFrame) -> (StateUpdate, EffectBatch) {
        let prompt_observed = matches!(
            &frame,
            DecodedFrame::Event(event) if matches!(event.event(), ShellEvent::PromptReady)
        );
        let update = self.shell.apply(frame);
        let mut effects = EffectBatch::default();

        match &update {
            StateUpdate::BufferSynchronized { .. } => {
                self.handle_authoritative_update(false, &mut effects);
            }
            StateUpdate::PromptReady { .. } => {
                self.handle_authoritative_update(true, &mut effects);
            }
            StateUpdate::WorkingDirectoryChanged(directory) => {
                if self.cwd != directory.as_path() {
                    self.cwd = directory.as_path().to_path_buf();
                    self.clear_completion(&mut effects);
                    self.query_restart_deferred = true;
                }
            }
            StateUpdate::ReloadRequested(_) => {
                self.clear_completion(&mut effects);
                self.clear_history_preview_authority();
            }
            StateUpdate::SnapshotRejected(_) => {
                self.clear_completion(&mut effects);
                self.clear_history_preview_authority();
                self.pending_replacement = None;
                self.issue_probe_if_safe(&mut effects, false);
            }
            StateUpdate::LifecycleSuppressed if prompt_observed && self.input_boundary_fence => {
                self.pending_replacement = None;
                self.clear_history_preview_authority();
                self.handle_suppressed_prompt(&mut effects);
            }
            StateUpdate::CommandStarted { .. }
            | StateUpdate::CommandStopped(_)
            | StateUpdate::CommandStoppedWithoutAttribution(_)
            | StateUpdate::CapabilityChanged(_)
            | StateUpdate::FrameRejected(_)
            | StateUpdate::LifecycleRejected(_)
            | StateUpdate::LifecycleSuppressed
            | StateUpdate::StreamOrderRejected { .. } => {
                self.clear_completion(&mut effects);
                self.pending_replacement = None;
                self.clear_history_preview_authority();
                if matches!(
                    &update,
                    StateUpdate::CommandStarted { .. }
                        | StateUpdate::CapabilityChanged(_)
                        | StateUpdate::FrameRejected(_)
                        | StateUpdate::LifecycleRejected(_)
                        | StateUpdate::StreamOrderRejected { .. }
                ) {
                    self.fence_prompt_observed = false;
                    self.fence_sync_nonce = None;
                }
            }
        }

        (update, effects)
    }

    /// Hides and cancels suggestions before asynchronous shell output, then
    /// requests a fresh snapshot if the shell remains at an editable prompt.
    #[must_use]
    pub fn observe_shell_output(&mut self) -> EffectBatch {
        let mut effects = EffectBatch::default();
        self.clear_completion(&mut effects);
        self.clear_history_preview_authority();
        self.probe_needed = true;
        self.issue_probe_if_safe(&mut effects, false);
        effects
    }

    /// Accepts one provider batch only for its exact active generation.
    pub fn accept_provider_batch(&mut self, batch: ProviderBatch) -> BatchOutcome {
        self.completion.accept_batch(batch)
    }

    /// Returns merged current-generation candidates for caller-owned ranking.
    ///
    /// # Errors
    ///
    /// Returns an authority rejection for stale, cancelled, or absent work.
    pub fn merged_candidates(
        &self,
        generation: u64,
    ) -> Result<Vec<Suggestion>, AuthorityRejection> {
        self.completion.merged_candidates(generation)
    }

    /// Applies a full ranked permutation and derives rendering or history recall.
    #[must_use]
    pub fn apply_ranked_candidates(
        &mut self,
        generation: u64,
        ranked: Vec<Suggestion>,
    ) -> PresentationReduction {
        let outcome = self.completion.apply_ranked(generation, ranked);
        let mut effects = EffectBatch::default();
        if matches!(outcome, PresentationOutcome::Applied { .. }) {
            if self.recall_history_when_ready {
                self.recall_history_when_ready = false;
                if !self.preview_selected_history(&mut effects) {
                    self.refresh_or_clear_overlay(&mut effects);
                }
            } else {
                self.refresh_or_clear_overlay(&mut effects);
            }
        }
        self.issue_probe_if_safe(&mut effects, false);
        PresentationReduction { outcome, effects }
    }

    /// Applies one background-validated alias edit through the normal inert
    /// shell-buffer replacement protocol.
    ///
    /// Stale, cancelled, malformed, or currently unsafe results are ignored.
    /// The returned batch is empty in those cases. A successful reduction
    /// clears completion UI, replaces the real shell-native edit buffer, and
    /// requests a correlated authoritative snapshot without executing it.
    #[must_use]
    pub fn apply_alias_expansion(&mut self, generation: u64, edit: TextEdit) -> EffectBatch {
        let mut effects = EffectBatch::default();
        if !self.actions_are_safe() {
            return effects;
        }
        let Some(query) = self.completion.active_query() else {
            return effects;
        };
        let TextEdit { range, replacement } = edit;
        if query.generation != generation
            || range.start > range.end
            || range.end > query.cursor
            || !query.line.is_char_boundary(range.start)
            || !query.line.is_char_boundary(range.end)
        {
            return effects;
        }

        let Some(removed_bytes) = range.end.checked_sub(range.start) else {
            return effects;
        };
        let Some(cursor) = query
            .cursor
            .checked_sub(removed_bytes)
            .and_then(|cursor| cursor.checked_add(replacement.len()))
        else {
            return effects;
        };
        let mut text = String::with_capacity(
            query
                .line
                .len()
                .saturating_sub(removed_bytes)
                .saturating_add(replacement.len()),
        );
        text.push_str(&query.line[..range.start]);
        text.push_str(&replacement);
        text.push_str(&query.line[range.end..]);
        let Ok(replacement) = BufferReplacement::new(text, cursor) else {
            return effects;
        };
        let _ = self.begin_replacement(replacement, ReplacementKind::AliasExpansion, &mut effects);
        effects
    }

    fn reduce_route_batch(&mut self, batch: RouteBatch) -> InputReduction {
        let consumed_bytes = batch.consumed_bytes();
        let mut effects = EffectBatch::default();

        for event in batch.into_events() {
            let (bytes, forwarding, action) = event.into_parts();
            self.release_unconfirmed_boundaries_for_local_toggle(action, &mut effects);
            let fenced_before = self.input_boundary_fence;
            let typed_alias_space =
                action == Some(InputAction::Printable(' ')) && self.actions_are_safe();
            let handled = action.is_some_and(|action| self.handle_action(action, &mut effects));
            let restoring_before_escape = action == Some(InputAction::Escape)
                && self
                    .pending_replacement
                    .as_ref()
                    .is_some_and(|pending| pending.kind == ReplacementKind::HistoryRestore);
            let forward = forwarding == Forwarding::Immediate
                || (forwarding == Forwarding::OnFallback && !handled);
            if !forward || bytes.is_empty() {
                continue;
            }

            let action = action.unwrap_or(InputAction::Desynchronize);
            if fenced_before {
                self.note_fenced_forward(action, &mut effects);
            } else if !matches!(action, InputAction::Enter | InputAction::CtrlC)
                && !restoring_before_escape
                && !matches!(action, InputAction::PasteStart | InputAction::PasteEnd)
            {
                self.observe_forwarded_edit(&mut effects, typed_alias_space);
            }
            effects.forward(bytes);
        }

        self.restart_deferred_query_if_safe(&mut effects);
        self.issue_probe_if_safe(&mut effects, true);

        debug_assert!(effects.len() <= MAX_SESSION_EFFECTS);
        InputReduction {
            consumed_bytes,
            effects,
        }
    }

    fn handle_action(&mut self, action: InputAction, effects: &mut EffectBatch) -> bool {
        if self.input_boundary_fence {
            return false;
        }
        match action {
            InputAction::Tab => self.handle_tab(effects),
            InputAction::Enter | InputAction::CtrlC => {
                self.handle_input_boundary(effects);
                false
            }
            InputAction::Escape => self.handle_escape(effects),
            InputAction::ArrowUp => self.handle_vertical_navigation(false, effects),
            InputAction::ArrowDown => self.handle_vertical_navigation(true, effects),
            InputAction::ArrowRight => self.accept_ghost(effects),
            InputAction::CtrlU => {
                self.clear_completion(effects);
                false
            }
            InputAction::ToggleMode => self.toggle_mode(effects),
            InputAction::ToggleMenu => self.toggle_menu(effects),
            InputAction::PasteStart => {
                self.paste_active = true;
                self.clear_completion(effects);
                false
            }
            InputAction::PasteEnd => {
                self.paste_active = false;
                false
            }
            InputAction::Desynchronize => {
                self.clear_completion(effects);
                self.clear_history_preview_authority();
                false
            }
            InputAction::Printable(_)
            | InputAction::ArrowLeft
            | InputAction::Backspace
            | InputAction::Delete
            | InputAction::Home
            | InputAction::End
            | InputAction::CtrlA
            | InputAction::CtrlE
            | InputAction::CtrlL
            | InputAction::CtrlW
            | InputAction::ShiftTab
            | InputAction::PasteData => false,
        }
    }

    fn handle_tab(&mut self, effects: &mut EffectBatch) -> bool {
        if !self.actions_are_safe() || !self.selection().layer_enabled() {
            return false;
        }
        if self.selection().is_visible() {
            if self.replace_with_selected(ReplacementKind::Acceptance, effects) {
                return true;
            }
            return false;
        }
        if self.selection().candidates().is_empty() {
            return false;
        }
        self.completion.note_buffer_changed();
        effects.push(SessionEffect::RefreshOverlay);
        true
    }

    fn handle_input_boundary(&mut self, effects: &mut EffectBatch) {
        let _ = self.completion.cancel_active_query();
        self.completion.dismiss_suggestions();
        self.clear_history_preview_authority();
        self.pending_replacement = None;
        self.input_boundary_fence = true;
        self.queued_input_boundaries = 1;
        self.fenced_input_pending = false;
        self.fence_prompt_observed = false;
        self.fence_sync_nonce = None;
        effects.push(SessionEffect::ClearOverlay);
    }

    fn handle_escape(&mut self, effects: &mut EffectBatch) -> bool {
        if self.mode == SessionMode::History
            && self.history_preview_active
            && self.actions_are_safe()
        {
            self.clear_completion(effects);
            self.mode = SessionMode::Spec;
            self.history_preview_active = false;
            self.recall_history_when_ready = false;
            effects.push(SessionEffect::ModeChanged(self.mode));
            if let Some(origin) = self.history_origin.take() {
                let _ = self.begin_replacement(origin, ReplacementKind::HistoryRestore, effects);
            }
            return false;
        }

        self.completion.dismiss_suggestions();
        effects.push(SessionEffect::ClearOverlay);
        false
    }

    fn handle_vertical_navigation(&mut self, down: bool, effects: &mut EffectBatch) -> bool {
        if !self.actions_are_safe() {
            return false;
        }
        if !self.selection().is_visible() {
            let empty_prompt = self
                .shell
                .buffer()
                .is_some_and(crate::shell_events::BufferSnapshot::is_empty);
            if empty_prompt {
                return self.enter_history_for_recall(effects);
            }
            return false;
        }

        if down {
            self.completion.select_next();
        } else {
            self.completion.select_previous();
        }
        if self.mode == SessionMode::History && self.preview_selected_history(effects) {
            return true;
        }
        effects.push(SessionEffect::RefreshOverlay);
        true
    }

    fn accept_ghost(&mut self, effects: &mut EffectBatch) -> bool {
        if !self.actions_are_safe() || !self.selection().is_visible() {
            return false;
        }
        let Some(query) = self.completion.active_query() else {
            return false;
        };
        if query.cursor != query.line.len() {
            return false;
        }
        let Some(candidate) = self.selection().selected() else {
            return false;
        };
        let Ok(result) = candidate.resulting_line(query) else {
            return false;
        };
        let Some(suffix) = ghost_suffix(&query.line, &result) else {
            return false;
        };
        let mut text = String::with_capacity(query.line.len().saturating_add(suffix.len()));
        text.push_str(&query.line);
        text.push_str(suffix);
        let cursor = text.len();
        let Ok(replacement) = BufferReplacement::new(text, cursor) else {
            return false;
        };
        self.begin_replacement(replacement, ReplacementKind::Ghost, effects)
    }

    fn toggle_mode(&mut self, effects: &mut EffectBatch) -> bool {
        if !self.actions_are_safe() && !self.can_restore_pending_history() {
            return false;
        }
        self.clear_completion(effects);
        match self.mode {
            SessionMode::Spec => {
                let Some(origin) = self.authoritative_replacement() else {
                    return false;
                };
                self.mode = SessionMode::History;
                self.history_origin = Some(origin);
                self.history_preview_active = false;
                self.recall_history_when_ready = false;
                effects.push(SessionEffect::ModeChanged(self.mode));
                self.start_query_from_authority(effects);
            }
            SessionMode::History => {
                self.mode = SessionMode::Spec;
                self.recall_history_when_ready = false;
                effects.push(SessionEffect::ModeChanged(self.mode));
                let origin = self.history_origin.take();
                let restore = self.history_preview_active
                    || self
                        .pending_replacement
                        .as_ref()
                        .is_some_and(|pending| pending.kind == ReplacementKind::HistoryPreview);
                self.history_preview_active = false;
                if restore {
                    let failed = match origin {
                        Some(origin) => !self.begin_replacement(
                            origin,
                            ReplacementKind::HistoryRestore,
                            effects,
                        ),
                        None => false,
                    };
                    if failed {
                        return false;
                    }
                } else {
                    self.start_query_from_authority(effects);
                }
            }
        }
        true
    }

    fn toggle_menu(&mut self, effects: &mut EffectBatch) -> bool {
        if !self.actions_are_safe() {
            return false;
        }
        self.completion.toggle_suggestion_layer();
        self.refresh_or_clear_overlay(effects);
        true
    }

    fn enter_history_for_recall(&mut self, effects: &mut EffectBatch) -> bool {
        let Some(origin) = self.authoritative_replacement() else {
            return false;
        };
        self.clear_completion(effects);
        self.mode = SessionMode::History;
        self.history_origin = Some(origin);
        self.history_preview_active = false;
        self.recall_history_when_ready = true;
        effects.push(SessionEffect::ModeChanged(self.mode));
        self.start_query_from_authority(effects);
        true
    }

    fn preview_selected_history(&mut self, effects: &mut EffectBatch) -> bool {
        if self.mode != SessionMode::History
            || self.selection().selected().map(Suggestion::source)
                != Some(SuggestionSource::History)
        {
            return false;
        }
        if self.history_origin.is_none() {
            self.history_origin = self.authoritative_replacement();
        }
        self.replace_with_selected(ReplacementKind::HistoryPreview, effects)
    }

    fn replace_with_selected(&mut self, kind: ReplacementKind, effects: &mut EffectBatch) -> bool {
        let Some(query) = self.completion.active_query().cloned() else {
            return false;
        };
        let Some(candidate) = self.selection().selected().cloned() else {
            return false;
        };
        let Ok(edit) = candidate.resolved_edit(&query.line) else {
            return false;
        };
        let cursor = edit.range.start.saturating_add(edit.replacement.len());
        let Ok(text) = edit.apply(&query.line) else {
            return false;
        };
        let Ok(replacement) = BufferReplacement::new(text, cursor) else {
            return false;
        };
        self.begin_replacement(replacement, kind, effects)
    }

    fn begin_replacement(
        &mut self,
        replacement: BufferReplacement,
        kind: ReplacementKind,
        effects: &mut EffectBatch,
    ) -> bool {
        if let Err(InputGenerationError::Exhausted) = self.shell.observe_local_input() {
            self.clear_completion(effects);
            effects.push(SessionEffect::Fault(SessionFault::InputGenerationExhausted));
            return false;
        }
        let _ = self.completion.cancel_active_query();
        self.completion.dismiss_suggestions();
        self.probe_needed = true;
        // A replacement may only reach the runtime together with its
        // correlated synchronization request; later events in the same batch
        // can legitimately suppress the batch-final request, so the pair is
        // emitted here or not at all.
        if self.shell.capability() != BufferSyncCapability::Probe {
            effects.push(SessionEffect::ClearOverlay);
            return false;
        }
        let nonce = match self.shell.begin_sync_probe() {
            Ok(nonce) => nonce,
            Err(ProbeRequestError::AlreadyPending | ProbeRequestError::NotAtEditablePrompt) => {
                effects.push(SessionEffect::ClearOverlay);
                return false;
            }
            Err(error) => {
                self.probe_needed = false;
                effects.push(SessionEffect::ClearOverlay);
                effects.push(SessionEffect::Fault(SessionFault::Probe(error)));
                return false;
            }
        };
        self.probe_needed = false;
        self.pending_replacement = Some(PendingReplacement {
            replacement: replacement.clone(),
            kind,
        });
        if matches!(
            kind,
            ReplacementKind::Acceptance | ReplacementKind::AliasExpansion | ReplacementKind::Ghost
        ) {
            self.clear_history_preview_authority();
        }
        effects.push(SessionEffect::ClearOverlay);
        effects.push(SessionEffect::ReplaceBuffer(replacement));
        effects.push(SessionEffect::RequestBufferSync(nonce));
        true
    }

    fn observe_forwarded_edit(&mut self, effects: &mut EffectBatch, typed_alias_space: bool) {
        if let Err(InputGenerationError::Exhausted) = self.shell.observe_local_input() {
            effects.push(SessionEffect::Fault(SessionFault::InputGenerationExhausted));
        }
        self.clear_completion(effects);
        self.alias_expansion_pending = typed_alias_space;
        self.pending_replacement = None;
        self.probe_needed = true;
        self.clear_history_preview_authority();
    }

    fn start_query_from_authority(&mut self, effects: &mut EffectBatch) {
        self.query_restart_deferred = false;
        let _ = self.completion.cancel_active_query();
        let alias_expansion = std::mem::take(&mut self.alias_expansion_pending);
        let Some((line, cursor)) = self.query_text_and_cursor() else {
            return;
        };
        if self.mode == SessionMode::Spec && line.is_empty() {
            effects.push(SessionEffect::ClearOverlay);
            return;
        }
        match self.completion.start_query(line, cursor, self.cwd.clone()) {
            Ok(work) => effects.push(SessionEffect::StartQuery {
                mode: self.mode,
                alias_expansion,
                work,
            }),
            Err(error) => effects.push(SessionEffect::Fault(SessionFault::QueryStart(error))),
        }
    }

    fn query_text_and_cursor(&self) -> Option<(String, usize)> {
        if let (SessionMode::History, true, Some(origin)) = (
            self.mode,
            self.history_preview_active,
            self.history_origin.as_ref(),
        ) {
            return Some((origin.as_str().to_owned(), origin.cursor()));
        }
        let buffer = self.shell.buffer()?;
        Some((buffer.as_str()?.to_owned(), buffer.cursor()))
    }

    fn authoritative_replacement(&self) -> Option<BufferReplacement> {
        let buffer = self.shell.buffer()?;
        BufferReplacement::new(buffer.as_str()?.to_owned(), buffer.cursor()).ok()
    }

    fn handle_authoritative_update(&mut self, prompt: bool, effects: &mut EffectBatch) {
        if !self.input_boundary_fence {
            self.probe_needed = false;
            self.confirm_or_discard_replacement();
            if prompt {
                self.clear_history_preview_authority();
            }
            self.start_query_from_authority(effects);
            return;
        }

        self.pending_replacement = None;
        if prompt {
            self.fence_sync_nonce = None;
            if self.consume_queued_boundary_prompt(effects) {
                return;
            }
            self.fence_prompt_observed = true;
            if self.fenced_input_pending {
                self.prepare_fence_probe(effects);
            } else {
                self.release_input_boundary(effects);
            }
            return;
        }

        let received_nonce = self
            .shell
            .buffer()
            .and_then(crate::shell_events::BufferSnapshot::probe_nonce);
        let causally_new = self
            .fence_sync_nonce
            .is_some_and(|expected| received_nonce == Some(expected));
        if causally_new && !self.fenced_input_pending {
            self.release_input_boundary(effects);
        } else {
            self.clear_completion(effects);
            if self.fence_prompt_observed {
                self.probe_needed = true;
                self.issue_probe_if_safe(effects, false);
            }
        }
    }

    fn handle_suppressed_prompt(&mut self, effects: &mut EffectBatch) {
        self.fence_sync_nonce = None;
        if self.consume_queued_boundary_prompt(effects) {
            return;
        }

        self.fence_prompt_observed = true;
        if self.fenced_input_pending {
            self.prepare_fence_probe(effects);
        } else {
            self.clear_completion(effects);
            self.probe_needed = true;
            self.issue_probe_if_safe(effects, false);
        }
    }

    fn consume_queued_boundary_prompt(&mut self, effects: &mut EffectBatch) -> bool {
        let Some(remaining) = self.queued_input_boundaries.checked_sub(1) else {
            return false;
        };
        self.queued_input_boundaries = remaining;
        self.fence_prompt_observed = remaining != 0;
        self.fence_sync_nonce = None;
        if remaining == 0 {
            return false;
        }
        self.probe_needed = false;
        self.clear_completion(effects);
        true
    }

    fn release_unconfirmed_boundaries_for_local_toggle(
        &mut self,
        action: Option<InputAction>,
        effects: &mut EffectBatch,
    ) {
        if !matches!(
            action,
            Some(InputAction::ToggleMode | InputAction::ToggleMenu)
        ) || !self.input_boundary_fence
            || !self.fence_prompt_observed
            || self.queued_input_boundaries == 0
            || self.fenced_input_pending
            || self.fence_sync_nonce.is_some()
            || !self.shell.suggestions_allowed()
        {
            return;
        }

        // Enter may continue a multiline command, and Ctrl+C may interrupt a
        // command whose start frame has not arrived yet. Treat both as raw
        // boundary hints until a distinct command start confirms more work.
        // Only local UI toggles may collapse the ambiguity. Forwarded input
        // remains fenced so a delayed queued command start cannot receive a
        // synchronization probe in its input stream.
        self.release_input_boundary(effects);
    }

    fn prepare_fence_probe(&mut self, effects: &mut EffectBatch) {
        self.clear_completion(effects);
        self.fenced_input_pending = false;
        match self.shell.observe_local_input() {
            Ok(_) => {
                self.probe_needed = true;
                self.issue_probe_if_safe(effects, false);
            }
            Err(InputGenerationError::Exhausted) => {
                effects.push(SessionEffect::Fault(SessionFault::InputGenerationExhausted));
            }
        }
    }

    fn release_input_boundary(&mut self, effects: &mut EffectBatch) {
        self.input_boundary_fence = false;
        self.queued_input_boundaries = 0;
        self.fenced_input_pending = false;
        self.fence_prompt_observed = false;
        self.fence_sync_nonce = None;
        self.probe_needed = false;
        self.start_query_from_authority(effects);
    }

    fn note_fenced_forward(&mut self, action: InputAction, effects: &mut EffectBatch) {
        let input_boundary = matches!(action, InputAction::Enter | InputAction::CtrlC);
        if input_boundary && self.shell.foreground() == ForegroundCommandState::Running {
            return;
        }

        if self.fence_sync_nonce.take().is_some() {
            if let Err(InputGenerationError::Exhausted) = self.shell.observe_local_input() {
                effects.push(SessionEffect::Fault(SessionFault::InputGenerationExhausted));
            }
            self.probe_needed = true;
        }

        if input_boundary {
            let Some(boundaries) = self.queued_input_boundaries.checked_add(1) else {
                self.fenced_input_pending = true;
                effects.push(SessionEffect::Fault(SessionFault::InputBoundaryExhausted));
                return;
            };
            self.queued_input_boundaries = boundaries;
            self.fenced_input_pending = false;
        } else {
            self.fenced_input_pending = true;
        }
    }

    fn confirm_or_discard_replacement(&mut self) {
        let Some(pending) = self.pending_replacement.take() else {
            return;
        };
        let matches = self.shell.buffer().is_some_and(|buffer| {
            buffer.as_bytes() == pending.replacement.as_bytes()
                && buffer.cursor() == pending.replacement.cursor()
        });
        if pending.kind == ReplacementKind::HistoryPreview {
            self.history_preview_active = matches;
            if !matches {
                self.history_origin = None;
            }
        }
    }

    fn clear_history_preview_authority(&mut self) {
        self.history_origin = None;
        self.history_preview_active = false;
        self.recall_history_when_ready = false;
    }

    fn actions_are_safe(&self) -> bool {
        self.shell.suggestions_allowed()
            && self.pending_replacement.is_none()
            && !self.paste_active
            && !self.input.is_bracketed_paste()
            && !self.input_boundary_fence
    }

    fn can_restore_pending_history(&self) -> bool {
        self.mode == SessionMode::History
            && self.history_origin.is_some()
            && self.shell.foreground() == ForegroundCommandState::Idle
            && self
                .pending_replacement
                .as_ref()
                .is_some_and(|pending| pending.kind == ReplacementKind::HistoryPreview)
    }

    fn clear_completion(&mut self, effects: &mut EffectBatch) {
        let _ = self.completion.cancel_active_query();
        self.completion.dismiss_suggestions();
        self.alias_expansion_pending = false;
        effects.push(SessionEffect::ClearOverlay);
    }

    fn refresh_or_clear_overlay(&self, effects: &mut EffectBatch) {
        if self.actions_are_safe() && self.selection().is_visible() {
            effects.push(SessionEffect::RefreshOverlay);
        } else {
            effects.push(SessionEffect::ClearOverlay);
        }
    }

    fn restart_deferred_query_if_safe(&mut self, effects: &mut EffectBatch) {
        if self.query_restart_deferred && self.actions_are_safe() {
            self.start_query_from_authority(effects);
        }
    }

    fn issue_probe_if_safe(&mut self, effects: &mut EffectBatch, require_complete_input: bool) {
        if !self.probe_needed
            || self.paste_active
            || self.input.is_bracketed_paste()
            || (require_complete_input && self.input.pending_len() != 0)
            || self.shell.capability() != BufferSyncCapability::Probe
            || (self.input_boundary_fence && !self.fence_prompt_observed)
            || self.queued_input_boundaries != 0
        {
            return;
        }
        match self.shell.begin_sync_probe() {
            Ok(nonce) => {
                self.probe_needed = false;
                if self.input_boundary_fence {
                    self.fence_sync_nonce = Some(nonce);
                    self.fenced_input_pending = false;
                }
                effects.push(SessionEffect::RequestBufferSync(nonce));
            }
            Err(ProbeRequestError::AlreadyPending | ProbeRequestError::NotAtEditablePrompt) => {}
            Err(error) => {
                self.probe_needed = false;
                effects.push(SessionEffect::Fault(SessionFault::Probe(error)));
            }
        }
    }
}

fn validate_cwd(cwd: PathBuf) -> Result<PathBuf, CwdUpdateError> {
    let bytes = cwd.as_os_str().as_encoded_bytes().len();
    if bytes > MAX_QUERY_CWD_BYTES {
        return Err(CwdUpdateError::TooLarge {
            bytes,
            limit: MAX_QUERY_CWD_BYTES,
        });
    }
    if !cwd.is_absolute() {
        return Err(CwdUpdateError::NotAbsolute);
    }
    Ok(cwd.into_boxed_path().into_path_buf())
}

#[cfg(test)]
mod tests {
    use crate::completion::{InsertionBehavior, TextEdit};
    use crate::coordinator::{BatchRejection, PresentationRejection};
    use crate::shell_events::ShellEventDecoder;

    use super::*;

    const PROVIDER: &str = "test";

    fn reducer() -> (SessionReducer, ShellEventDecoder) {
        let reducer = SessionReducer::new(
            StreamEpoch::INITIAL,
            b"\x12",
            b"\x10",
            [PROVIDER],
            10,
            "/tmp",
        )
        .unwrap();
        (reducer, ShellEventDecoder::new(StreamEpoch::INITIAL))
    }

    fn apply_wire(
        reducer: &mut SessionReducer,
        decoder: &mut ShellEventDecoder,
        wire: &[u8],
    ) -> Vec<EffectBatch> {
        let mut frames = Vec::new();
        decoder.push(wire, |frame| frames.push(frame));
        frames
            .into_iter()
            .map(|frame| reducer.apply_shell_frame(frame).1)
            .collect()
    }

    fn ready() -> (SessionReducer, ShellEventDecoder) {
        let (mut reducer, mut decoder) = reducer();
        apply_wire(
            &mut reducer,
            &mut decoder,
            b"capability:sync-probe:0\0prompt-ready\0",
        );
        (reducer, decoder)
    }

    fn synchronize(
        reducer: &mut SessionReducer,
        decoder: &mut ShellEventDecoder,
        typed: &[u8],
        line: &str,
        cursor: usize,
    ) -> u64 {
        let reduction = reducer.route_input(typed);
        let nonce = reduction
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        let frame = format!("probe-buffer:b:{nonce}:{cursor}:{line}\0");
        apply_wire(reducer, decoder, frame.as_bytes());
        reducer.active_query().unwrap().generation
    }

    fn suggestion(
        line: &str,
        replacement: &str,
        source: SuggestionSource,
        insertion: InsertionBehavior,
    ) -> Suggestion {
        Suggestion::new(
            TextEdit {
                range: 0..line.len(),
                replacement: replacement.into(),
            },
            replacement,
            "description",
            "command",
            source,
            insertion,
            replacement,
        )
    }

    fn present(reducer: &mut SessionReducer, generation: u64, candidate: Suggestion) {
        assert!(matches!(
            reducer.accept_provider_batch(ProviderBatch::success(
                PROVIDER,
                generation,
                vec![candidate.clone()]
            )),
            BatchOutcome::Accepted(_)
        ));
        assert!(matches!(
            reducer
                .apply_ranked_candidates(generation, vec![candidate])
                .outcome(),
            PresentationOutcome::Applied { .. }
        ));
    }

    fn query_effect(effects: &[EffectBatch]) -> (&QueryWork, bool) {
        effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::StartQuery {
                    work,
                    alias_expansion,
                    ..
                } => Some((work, *alias_expansion)),
                _ => None,
            })
            .unwrap()
    }

    fn request_history_preview(reducer: &mut SessionReducer, original: &str, preview: &str) -> u64 {
        let toggle = reducer.route_input(b"\x12");
        let generation = toggle
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { mode, work, .. } if *mode == SessionMode::History => {
                    Some(work.query().generation)
                }
                _ => None,
            })
            .unwrap();
        present(
            reducer,
            generation,
            suggestion(
                original,
                preview,
                SuggestionSource::History,
                InsertionBehavior::Exact,
            ),
        );
        let preview = reducer.route_input(b"\x1b[A");
        preview
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap()
    }

    fn forwarded(batch: &EffectBatch) -> Vec<u8> {
        batch
            .effects()
            .iter()
            .filter_map(|effect| match effect {
                SessionEffect::ForwardInput(bytes) => Some(bytes.as_ref()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn tab_replaces_then_subsequent_enter_executes_without_forwarding_tab() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"git che", "git che", 7);
        present(
            &mut reducer,
            generation,
            suggestion(
                "git che",
                "git checkout",
                SuggestionSource::Spec,
                InsertionBehavior::AppendSpace,
            ),
        );

        let reduction = reducer.route_input(b"\t\r");
        let effects = reduction.effects().effects();
        let replacement = effects.iter().position(|effect| {
            matches!(
                effect,
                SessionEffect::ReplaceBuffer(replacement)
                    if replacement.as_str() == "git checkout "
                        && replacement.cursor() == "git checkout ".len()
            )
        });
        let enter = effects.iter().position(
            |effect| matches!(effect, SessionEffect::ForwardInput(bytes) if bytes.as_ref() == b"\r"),
        );
        let sync = effects
            .iter()
            .position(|effect| matches!(effect, SessionEffect::RequestBufferSync(_)));
        assert!(replacement.is_some_and(|replacement| {
            sync.is_some_and(|sync| enter.is_some_and(|enter| replacement < sync && sync < enter))
        }));
        assert_eq!(forwarded(reduction.effects()), b"\r");
    }

    #[test]
    fn finish_input_forwards_a_retained_partial_sequence_intact() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);

        let retained = reducer.route_input(b"\x1b[");
        assert!(
            !retained
                .effects()
                .effects()
                .iter()
                .any(|effect| matches!(effect, SessionEffect::ForwardInput(_)))
        );

        let reduction = reducer.finish_input();
        let forwarded: Vec<u8> = reduction
            .effects()
            .effects()
            .iter()
            .filter_map(|effect| match effect {
                SessionEffect::ForwardInput(bytes) => Some(bytes.as_ref().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(forwarded, b"\x1b[");
    }

    #[test]
    fn history_preview_and_enter_in_one_batch_pair_replacement_with_sync() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
        let toggle = reducer.route_input(b"\x12");
        let generation = toggle
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { mode, work, .. } if *mode == SessionMode::History => {
                    Some(work.query().generation)
                }
                _ => None,
            })
            .unwrap();
        present(
            &mut reducer,
            generation,
            suggestion(
                "git",
                "git status",
                SuggestionSource::History,
                InsertionBehavior::Exact,
            ),
        );

        let reduction = reducer.route_input(b"\x1b[A\r");
        let effects = reduction.effects().effects();
        let replacement = effects.iter().position(|effect| {
            matches!(
                effect,
                SessionEffect::ReplaceBuffer(replacement)
                    if replacement.as_str() == "git status"
            )
        });
        let sync = effects
            .iter()
            .position(|effect| matches!(effect, SessionEffect::RequestBufferSync(_)));
        let enter = effects.iter().position(
            |effect| matches!(effect, SessionEffect::ForwardInput(bytes) if bytes.as_ref() == b"\r"),
        );
        assert!(replacement.is_some_and(|replacement| {
            sync.is_some_and(|sync| enter.is_some_and(|enter| replacement < sync && sync < enter))
        }));
    }

    #[test]
    fn enter_executes_original_buffer_not_highlighted_candidate() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"git che", "git che", 7);
        present(
            &mut reducer,
            generation,
            suggestion(
                "git che",
                "git cherry-pick",
                SuggestionSource::Spec,
                InsertionBehavior::Exact,
            ),
        );

        let reduction = reducer.route_input(b"\r");
        assert_eq!(forwarded(reduction.effects()), b"\r");
        assert!(
            !reduction
                .effects()
                .effects()
                .iter()
                .any(|effect| matches!(effect, SessionEffect::ReplaceBuffer(_)))
        );
        let clear = reduction
            .effects()
            .effects()
            .iter()
            .position(|effect| matches!(effect, SessionEffect::ClearOverlay));
        let enter = reduction.effects().effects().iter().position(
            |effect| matches!(effect, SessionEffect::ForwardInput(bytes) if bytes.as_ref() == b"\r"),
        );
        assert!(clear.is_some_and(|clear| enter.is_some_and(|enter| clear < enter)));
    }

    #[test]
    fn bracketed_paste_is_forwarded_byte_exact_without_acceptance() {
        let (mut reducer, _) = ready();
        let paste = b"\x1b[200~g\nwhoami && ll\x1b[201~";
        let reduction = reducer.route_input(paste);
        assert_eq!(forwarded(reduction.effects()), paste);
        assert!(!reduction.effects().effects().iter().any(|effect| {
            matches!(
                effect,
                SessionEffect::ReplaceBuffer(_) | SessionEffect::StartQuery { .. }
            )
        }));
    }

    #[test]
    fn unicode_character_cursor_becomes_authoritative_byte_cursor() {
        let (mut reducer, mut decoder) = ready();
        let reduction = reducer.route_input(b"x");
        let nonce = reduction
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        let frame = format!("probe-buffer:c:{nonce}:4:echo \u{4e16}\u{754c}\0");
        apply_wire(&mut reducer, &mut decoder, frame.as_bytes());

        let query = reducer.active_query().unwrap();
        assert_eq!(query.line, "echo \u{4e16}\u{754c}");
        assert_eq!(query.cursor, 4);
        assert_eq!(query.prefix(), "echo");
    }

    #[test]
    fn late_provider_results_cannot_restore_old_selection() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"g", "g", 1);
        let candidate = suggestion("g", "git", SuggestionSource::Spec, InsertionBehavior::Exact);
        let _ = reducer.route_input(b"i");

        assert!(matches!(
            reducer.accept_provider_batch(ProviderBatch::success(
                PROVIDER,
                generation,
                vec![candidate.clone()]
            )),
            BatchOutcome::Rejected(BatchRejection::Authority(_))
        ));
        assert!(matches!(
            reducer
                .apply_ranked_candidates(generation, vec![candidate])
                .outcome(),
            PresentationOutcome::Rejected(PresentationRejection::Authority(_))
        ));
        assert!(!reducer.selection().is_visible());
    }

    #[test]
    fn mismatching_history_preview_ack_uses_the_accepted_shell_buffer() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
        let nonce = request_history_preview(&mut reducer, "git", "git status");
        assert!(!reducer.history_preview_active);

        let frame = format!("probe-buffer:b:{nonce}:10:git branch\0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        let query = effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { mode, work, .. } if *mode == SessionMode::History => {
                    Some(work.query())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(query.line, "git branch");
        assert_eq!(query.cursor, 10);
        assert!(!reducer.history_preview_active);
        assert!(reducer.history_origin.is_none());
    }

    #[test]
    fn prompt_ready_replaces_acknowledged_history_preview_authority_with_empty_text() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
        let nonce = request_history_preview(&mut reducer, "git", "git status");
        let frame = format!("probe-buffer:b:{nonce}:10:git status\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(reducer.history_preview_active);

        let effects = apply_wire(&mut reducer, &mut decoder, b"prompt-ready\0");
        let query = effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { mode, work, .. } if *mode == SessionMode::History => {
                    Some(work.query())
                }
                _ => None,
            })
            .unwrap();
        assert!(query.line.is_empty());
        assert_eq!(query.cursor, 0);
        assert!(!reducer.history_preview_active);
        assert!(reducer.history_origin.is_none());
    }

    #[test]
    fn history_preview_authority_is_dropped_after_rejected_frame() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
        let nonce = request_history_preview(&mut reducer, "git", "git status");
        let frame = format!("probe-buffer:b:{nonce}:10:git status\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(reducer.history_preview_active);

        let _ = apply_wire(&mut reducer, &mut decoder, b"not-an-event\0");
        assert!(!reducer.history_preview_active);
        assert!(reducer.history_origin.is_none());

        let pending = reducer.route_input(b"\x1b");
        assert!(pending.effects().is_empty());
        let escape = reducer.flush_input();
        assert_eq!(forwarded(escape.effects()), b"\x1b");
        assert!(
            !escape
                .effects()
                .effects()
                .iter()
                .any(|effect| matches!(effect, SessionEffect::ReplaceBuffer(_)))
        );
    }

    #[test]
    fn cursor_actions_abandon_history_origin_before_resynchronizing() {
        let cases: &[(&[u8], usize)] = &[
            (b"\x1b[D", 9),
            (b"\x1b[H", 0),
            (b"\x1b[F", 10),
            (b"\x01", 0),
            (b"\x05", 10),
        ];

        for &(input, cursor) in cases {
            let (mut reducer, mut decoder) = ready();
            let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
            let preview_nonce = request_history_preview(&mut reducer, "git", "git status");
            let frame = format!("probe-buffer:b:{preview_nonce}:10:git status\0");
            let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
            assert!(reducer.history_preview_active);

            let action = reducer.route_input(input);
            assert_eq!(forwarded(action.effects()), input);
            assert!(!reducer.history_preview_active);
            assert!(reducer.history_origin.is_none());
            let nonce = action
                .effects()
                .effects()
                .iter()
                .find_map(|effect| match effect {
                    SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                    _ => None,
                })
                .unwrap();
            let frame = format!("probe-buffer:b:{nonce}:{cursor}:git status\0");
            let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
            let query = reducer.active_query().unwrap();
            assert_eq!(query.line, "git status");
            assert_eq!(query.cursor, cursor);
        }
    }

    #[test]
    fn enter_does_not_admit_a_preboundary_probe_response() {
        let (mut reducer, mut decoder) = ready();
        let typed = reducer.route_input(b"g");
        let old_nonce = typed
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();

        let enter = reducer.route_input(b"\r");
        assert_eq!(forwarded(enter.effects()), b"\r");
        assert!(reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());
        let clear = enter
            .effects()
            .effects()
            .iter()
            .position(|effect| matches!(effect, SessionEffect::ClearOverlay));
        let forwarded = enter.effects().effects().iter().position(
            |effect| matches!(effect, SessionEffect::ForwardInput(bytes) if bytes.as_ref() == b"\r"),
        );
        assert!(clear.is_some_and(|clear| forwarded.is_some_and(|forwarded| clear < forwarded)));

        let frame = format!("probe-buffer:b:{old_nonce}:1:g\0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());
        assert!(
            effects
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| !matches!(effect, SessionEffect::StartQuery { .. }))
        );
    }

    #[test]
    fn typed_alias_space_requests_background_validation_and_inert_replacement() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"gs", "gs", 2);

        let space = reducer.route_input(b" ");
        assert_eq!(forwarded(space.effects()), b" ");
        let nonce = space
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        let frame = format!("probe-buffer:b:{nonce}:3:gs \0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        let (work, alias_expansion) = query_effect(&effects);
        assert!(alias_expansion);
        assert_eq!(work.query().line, "gs ");
        let generation = work.query().generation;

        let replacement = reducer.apply_alias_expansion(
            generation,
            TextEdit {
                range: 0..2,
                replacement: "git status".into(),
            },
        );
        assert!(matches!(
            replacement.effects(),
            [
                SessionEffect::ClearOverlay,
                SessionEffect::ReplaceBuffer(value),
                SessionEffect::RequestBufferSync(_)
            ] if value.as_str() == "git status " && value.cursor() == 11
        ));
        assert!(reducer.replacement_pending());
        assert!(reducer.active_query().is_none());
    }

    #[test]
    fn pasted_space_never_requests_alias_expansion() {
        let (mut reducer, mut decoder) = ready();
        let paste = reducer.route_input(b"\x1b[200~gs \x1b[201~");
        assert_eq!(forwarded(paste.effects()), b"\x1b[200~gs \x1b[201~");
        let nonce = paste
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        let frame = format!("probe-buffer:b:{nonce}:3:gs \0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        let (_, alias_expansion) = query_effect(&effects);
        assert!(!alias_expansion);
    }

    #[test]
    fn stale_or_cancelled_alias_expansion_is_ignored() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"gs ", "gs ", 3);
        let edit = TextEdit {
            range: 0..2,
            replacement: "git status".into(),
        };

        assert!(
            reducer
                .apply_alias_expansion(generation.saturating_add(1), edit.clone())
                .is_empty()
        );
        let _ = reducer.observe_shell_output();
        assert!(reducer.apply_alias_expansion(generation, edit).is_empty());
        assert!(!reducer.replacement_pending());
    }

    #[test]
    fn shell_redraw_at_editable_prompt_resynchronizes_the_active_buffer() {
        let (mut reducer, mut decoder) = ready();
        let first_generation = synchronize(&mut reducer, &mut decoder, b"ssh-", "ssh-", 4);

        let effects = reducer.observe_shell_output();
        let nonce = effects
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        assert!(reducer.active_query().is_none());

        let frame = format!("probe-buffer:b:{nonce}:4:ssh-\0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        let (work, _) = query_effect(&effects);

        assert_eq!(work.query().line, "ssh-");
        assert!(work.query().generation > first_generation);
    }

    #[test]
    fn prompt_after_fenced_typeahead_requires_a_newer_probe() {
        let (mut reducer, mut decoder) = ready();
        let enter = reducer.route_input(b"\r");
        assert_eq!(forwarded(enter.effects()), b"\r");
        let typeahead = reducer.route_input(b"x");
        assert_eq!(forwarded(typeahead.effects()), b"x");
        assert!(reducer.fenced_input_pending);

        let effects = apply_wire(&mut reducer, &mut decoder, b"prompt-ready\0");
        let nonce = effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        assert!(reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());

        let frame = format!("probe-buffer:b:{nonce}:1:x\0");
        let effects = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        let query = effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { work, .. } => Some(work.query()),
                _ => None,
            })
            .unwrap();
        assert_eq!(query.line, "x");
        assert_eq!(query.cursor, 1);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn enter_fences_same_batch_and_later_actions_until_causal_sync() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"g", "g", 1);
        present(
            &mut reducer,
            generation,
            suggestion("g", "git", SuggestionSource::Spec, InsertionBehavior::Exact),
        );

        let same_read = b"\r\x12\x10\t\x1b[Z";
        let reduction = reducer.route_input(same_read);
        assert_eq!(forwarded(reduction.effects()), same_read);
        assert!(reducer.input_boundary_fence);
        assert!(!reduction.effects().effects().iter().any(|effect| {
            matches!(
                effect,
                SessionEffect::ModeChanged(_)
                    | SessionEffect::ReplaceBuffer(_)
                    | SessionEffect::RefreshOverlay
                    | SessionEffect::StartQuery { .. }
                    | SessionEffect::RequestBufferSync(_)
            )
        }));

        let later_read = b"\x12\x10\t\x1b[Z";
        let reduction = reducer.route_input(later_read);
        assert_eq!(forwarded(reduction.effects()), later_read);
        assert_eq!(reduction.effects().len(), 1);
        assert!(reducer.input_boundary_fence);

        let effects = apply_wire(&mut reducer, &mut decoder, b"prompt-ready\0");
        let nonce = effects
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        assert!(reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());

        let frame = format!("probe-buffer:b:{nonce}:0:\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(!reducer.input_boundary_fence);
        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
    }

    #[test]
    fn queued_command_boundary_never_injects_a_probe_into_the_next_command() {
        let (mut reducer, mut decoder) = ready();

        let queued = reducer.route_input(b"\recho hi\r");
        assert_eq!(forwarded(queued.effects()), b"\recho hi\r");
        assert_eq!(reducer.queued_input_boundaries, 2);
        assert!(!reducer.fenced_input_pending);
        assert!(queued.effects().effects().iter().all(|effect| {
            !matches!(
                effect,
                SessionEffect::RequestBufferSync(_) | SessionEffect::StartQuery { .. }
            )
        }));

        let lifecycle = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:first\0command-stop:0\0prompt-ready\0command-start:echo hi\0",
        );
        assert_eq!(lifecycle.len(), 4);
        assert!(
            lifecycle
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| {
                    !matches!(
                        effect,
                        SessionEffect::RequestBufferSync(_) | SessionEffect::StartQuery { .. }
                    )
                })
        );
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(reducer.input_boundary_fence);
        assert_eq!(
            reducer.shell().foreground(),
            ForegroundCommandState::Running
        );

        let final_prompt = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-stop:0\0prompt-ready\0",
        );
        assert!(
            final_prompt
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert_eq!(reducer.queued_input_boundaries, 0);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn forwarded_input_before_a_delayed_queued_start_stays_fenced() {
        let (mut reducer, mut decoder) = ready();
        let queued = reducer.route_input(b"first\rsecond\r");
        assert_eq!(forwarded(queued.effects()), b"first\rsecond\r");

        let first_prompt = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:first\0command-stop:0\0prompt-ready\0",
        );
        assert!(
            first_prompt
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert!(reducer.fence_prompt_observed);

        let typed = reducer.route_input(b"x");
        assert_eq!(forwarded(typed.effects()), b"x");
        assert!(typed.effects().effects().iter().all(|effect| {
            !matches!(
                effect,
                SessionEffect::RequestBufferSync(_)
                    | SessionEffect::StartQuery { .. }
                    | SessionEffect::ReplaceBuffer(_)
            )
        }));
        assert!(reducer.input_boundary_fence);
        assert!(reducer.fenced_input_pending);

        let final_prompt = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:second\0command-stop:0\0prompt-ready\0",
        );
        assert!(
            final_prompt
                .iter()
                .take(2)
                .flat_map(EffectBatch::effects)
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert!(
            final_prompt
                .last()
                .unwrap()
                .effects()
                .iter()
                .any(|effect| matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert!(reducer.input_boundary_fence);
    }

    #[test]
    fn multiline_enters_collapse_for_a_local_toggle_at_the_stable_prompt() {
        let (mut reducer, mut decoder) = ready();
        let input = b"echo \"Troy\rBarnes\"\r";
        let multiline = reducer.route_input(input);
        assert_eq!(forwarded(multiline.effects()), input);
        assert_eq!(reducer.queued_input_boundaries, 2);

        let lifecycle = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:echo Troy Barnes\0command-stop:0\0prompt-ready\0",
        );
        assert!(
            lifecycle
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| {
                    !matches!(
                        effect,
                        SessionEffect::RequestBufferSync(_) | SessionEffect::StartQuery { .. }
                    )
                })
        );
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(reducer.fence_prompt_observed);

        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
        assert_eq!(reducer.queued_input_boundaries, 0);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn prestart_ctrl_c_does_not_predict_a_second_prompt() {
        let (mut reducer, mut decoder) = ready();
        let input = b"sleep 10\r\x03";
        let interrupted = reducer.route_input(input);
        assert_eq!(forwarded(interrupted.effects()), input);
        assert_eq!(reducer.queued_input_boundaries, 2);

        let lifecycle = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:sleep 10\0command-stop:130\0prompt-ready\0",
        );
        assert!(
            lifecycle
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| {
                    !matches!(
                        effect,
                        SessionEffect::RequestBufferSync(_) | SessionEffect::StartQuery { .. }
                    )
                })
        );
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(reducer.fence_prompt_observed);

        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
        assert_eq!(reducer.queued_input_boundaries, 0);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn prestart_ctrl_c_in_a_later_read_has_the_same_single_prompt_lifecycle() {
        let (mut reducer, mut decoder) = ready();
        let command = reducer.route_input(b"sleep 10\r");
        let interrupted = reducer.route_input(b"\x03");
        assert_eq!(forwarded(command.effects()), b"sleep 10\r");
        assert_eq!(forwarded(interrupted.effects()), b"\x03");
        assert_eq!(reducer.queued_input_boundaries, 2);

        let _ = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:sleep 10\0command-stop:130\0prompt-ready\0",
        );
        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn multiline_then_queued_command_stays_fenced_through_both_lifecycles() {
        let (mut reducer, mut decoder) = ready();
        let input = b"echo \"Troy\rBarnes\"\recho hi\r";
        let queued = reducer.route_input(input);
        assert_eq!(forwarded(queued.effects()), input);
        assert_eq!(reducer.queued_input_boundaries, 3);

        let lifecycles = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:echo Troy Barnes\0command-stop:0\0prompt-ready\0\
              command-start:echo hi\0command-stop:0\0prompt-ready\0",
        );
        assert!(
            lifecycles
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| {
                    !matches!(
                        effect,
                        SessionEffect::RequestBufferSync(_) | SessionEffect::StartQuery { .. }
                    )
                })
        );
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(reducer.fence_prompt_observed);

        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn queued_enter_while_probe_is_pending_does_not_request_a_new_probe() {
        let (mut reducer, mut decoder) = ready();
        let _ = reducer.route_input(b"\r");
        let _ = reducer.route_input(b"x");
        let prompt = apply_wire(&mut reducer, &mut decoder, b"prompt-ready\0");
        let nonce = prompt
            .iter()
            .flat_map(EffectBatch::effects)
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();

        let queued = reducer.route_input(b"\r");
        assert_eq!(forwarded(queued.effects()), b"\r");
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(
            queued
                .effects()
                .effects()
                .iter()
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );

        let stale = format!("probe-buffer:b:{nonce}:1:x\0");
        let mut frames = Vec::new();
        decoder.push(stale.as_bytes(), |frame| frames.push(frame));
        let (update, effects) = reducer.apply_shell_frame(frames.pop().unwrap());
        assert!(matches!(update, StateUpdate::SnapshotRejected(_)));
        assert!(
            effects
                .effects()
                .iter()
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert_eq!(reducer.queued_input_boundaries, 1);
        assert!(reducer.input_boundary_fence);

        let final_prompt = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-start:x\0command-stop:0\0prompt-ready\0",
        );
        assert!(
            final_prompt
                .iter()
                .flat_map(EffectBatch::effects)
                .all(|effect| !matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert_eq!(reducer.queued_input_boundaries, 0);
        assert!(!reducer.input_boundary_fence);
    }

    #[test]
    fn ctrl_c_while_foreground_does_not_add_an_extra_prompt_boundary() {
        let (mut reducer, mut decoder) = ready();
        let command = reducer.route_input(b"sleep 10\r");
        assert_eq!(forwarded(command.effects()), b"sleep 10\r");
        let _ = apply_wire(&mut reducer, &mut decoder, b"command-start:sleep 10\0");
        assert_eq!(
            reducer.shell().foreground(),
            ForegroundCommandState::Running
        );
        assert_eq!(reducer.queued_input_boundaries, 1);

        let interrupted = reducer.route_input(b"\x03");
        assert_eq!(forwarded(interrupted.effects()), b"\x03");
        assert_eq!(reducer.queued_input_boundaries, 1);

        let _ = apply_wire(
            &mut reducer,
            &mut decoder,
            b"command-stop:130\0prompt-ready\0",
        );
        assert_eq!(reducer.queued_input_boundaries, 0);
        assert!(!reducer.input_boundary_fence);

        let toggle = reducer.route_input(b"\x12");
        assert!(forwarded(toggle.effects()).is_empty());
        assert_eq!(reducer.mode(), SessionMode::History);
    }

    #[test]
    fn ctrl_c_fences_later_mode_toggle_until_causal_sync() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);

        let interrupt = reducer.route_input(b"\x03");
        assert_eq!(forwarded(interrupt.effects()), b"\x03");
        assert!(reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());

        let toggle = reducer.route_input(b"\x12");
        assert_eq!(forwarded(toggle.effects()), b"\x12");
        assert_eq!(reducer.mode(), SessionMode::Spec);
        assert!(reducer.active_query().is_none());
        assert!(!toggle.effects().effects().iter().any(|effect| {
            matches!(
                effect,
                SessionEffect::ModeChanged(_) | SessionEffect::StartQuery { .. }
            )
        }));

        let effects = apply_wire(&mut reducer, &mut decoder, b"prompt-ready\0");
        assert!(reducer.input_boundary_fence);
        assert!(
            effects
                .iter()
                .flat_map(EffectBatch::effects)
                .any(|effect| matches!(effect, SessionEffect::RequestBufferSync(_)))
        );
        assert!(reducer.active_query().is_none());
    }

    #[test]
    fn ctrl_c_after_unacknowledged_input_resynchronizes_at_the_suppressed_prompt() {
        let (mut reducer, mut decoder) = ready();

        let interrupted = reducer.route_input(b"x\x03");
        assert_eq!(forwarded(interrupted.effects()), b"x\x03");
        assert!(reducer.input_boundary_fence);
        assert!(reducer.probe_needed);
        assert!(
            interrupted
                .effects()
                .effects()
                .iter()
                .all(|effect| { !matches!(effect, SessionEffect::RequestBufferSync(_)) })
        );

        let mut frames = Vec::new();
        decoder.push(b"prompt-ready\0", |frame| frames.push(frame));
        let (update, effects) = reducer.apply_shell_frame(frames.pop().unwrap());
        assert_eq!(update, StateUpdate::LifecycleSuppressed);
        let nonce = effects
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        assert!(
            effects
                .effects()
                .iter()
                .all(|effect| !matches!(effect, SessionEffect::StartQuery { .. }))
        );
        assert!(reducer.input_boundary_fence);

        let frame = format!("probe-buffer:b:{nonce}:0:\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(!reducer.input_boundary_fence);
        assert!(reducer.active_query().is_none());
    }

    #[test]
    fn shift_tab_falls_back_unless_it_is_the_configured_menu_binding() {
        let (mut reducer, mut decoder) = ready();
        let generation = synchronize(&mut reducer, &mut decoder, b"g", "g", 1);
        present(
            &mut reducer,
            generation,
            suggestion("g", "git", SuggestionSource::Spec, InsertionBehavior::Exact),
        );
        let fallback = reducer.route_input(b"\x1b[Z");
        assert_eq!(forwarded(fallback.effects()), b"\x1b[Z");
        assert!(reducer.selection().layer_enabled());
        assert!(!fallback.effects().effects().iter().any(|effect| {
            matches!(
                effect,
                SessionEffect::ModeChanged(_)
                    | SessionEffect::ReplaceBuffer(_)
                    | SessionEffect::RefreshOverlay
            )
        }));

        let mut configured = SessionReducer::new(
            StreamEpoch::INITIAL,
            b"\x12",
            b"\x1b[Z",
            [PROVIDER],
            10,
            "/tmp",
        )
        .unwrap();
        let mut configured_decoder = ShellEventDecoder::new(StreamEpoch::INITIAL);
        let _ = apply_wire(
            &mut configured,
            &mut configured_decoder,
            b"capability:sync-probe:0\0prompt-ready\0",
        );
        let handled = configured.route_input(b"\x1b[Z");
        assert!(forwarded(handled.effects()).is_empty());
        assert!(!configured.selection().layer_enabled());
    }

    #[test]
    fn cwd_update_is_bounded_atomic_and_restarts_safe_query_authority() {
        let (mut reducer, mut decoder) = ready();
        let old_generation = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);

        let effects = reducer.update_cwd("/var/tmp").unwrap();
        let work = effects
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { work, .. } => Some(work),
                _ => None,
            })
            .unwrap();
        assert_eq!(work.query().cwd, Path::new("/var/tmp"));
        assert_eq!(reducer.cwd(), Path::new("/var/tmp"));
        assert_ne!(work.query().generation, old_generation);
        assert!(matches!(
            reducer.accept_provider_batch(ProviderBatch::success(
                PROVIDER,
                old_generation,
                Vec::new()
            )),
            BatchOutcome::Rejected(BatchRejection::Authority(_))
        ));

        let generation = reducer.active_query().unwrap().generation;
        assert_eq!(
            reducer.update_cwd("relative/path").unwrap_err(),
            CwdUpdateError::NotAbsolute
        );
        assert_eq!(reducer.cwd(), Path::new("/var/tmp"));
        assert_eq!(reducer.active_query().unwrap().generation, generation);

        let oversized = format!("/{}", "a".repeat(MAX_QUERY_CWD_BYTES));
        assert!(matches!(
            reducer.update_cwd(oversized),
            Err(CwdUpdateError::TooLarge { .. })
        ));
        assert_eq!(reducer.cwd(), Path::new("/var/tmp"));
        assert_eq!(reducer.active_query().unwrap().generation, generation);
    }

    #[test]
    fn cwd_restart_deferred_by_empty_paste_runs_when_paste_ends() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);

        let start = reducer.route_input(b"\x1b[200~");
        assert_eq!(forwarded(start.effects()), b"\x1b[200~");
        let update = reducer.update_cwd("/var/tmp").unwrap();
        assert!(reducer.query_restart_deferred());
        assert!(
            update
                .effects()
                .iter()
                .all(|effect| !matches!(effect, SessionEffect::StartQuery { .. }))
        );

        let end = reducer.route_input(b"\x1b[201~");
        assert_eq!(forwarded(end.effects()), b"\x1b[201~");
        let work = end
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { work, .. } => Some(work),
                _ => None,
            })
            .unwrap();
        assert_eq!(work.query().line, "git");
        assert_eq!(work.query().cursor, 3);
        assert_eq!(work.query().cwd, Path::new("/var/tmp"));
        assert!(!reducer.query_restart_deferred());
    }

    #[test]
    fn leaving_history_restores_original_buffer_after_preview() {
        let (mut reducer, mut decoder) = ready();
        let original_generation =
            synchronize(&mut reducer, &mut decoder, b"git", "git", "git".len());
        assert_eq!(
            reducer.active_query().unwrap().generation,
            original_generation
        );

        let toggle = reducer.route_input(b"\x12");
        let history_generation = toggle
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::StartQuery { mode, work, .. } if *mode == SessionMode::History => {
                    Some(work.query().generation)
                }
                _ => None,
            })
            .unwrap();
        let history = suggestion(
            "git",
            "git status",
            SuggestionSource::History,
            InsertionBehavior::Exact,
        );
        present(&mut reducer, history_generation, history);

        let preview = reducer.route_input(b"\x1b[A");
        let nonce = preview
            .effects()
            .effects()
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        assert!(preview.effects().effects().iter().any(|effect| {
            matches!(effect, SessionEffect::ReplaceBuffer(value) if value.as_str() == "git status")
        }));
        let frame = format!("probe-buffer:b:{nonce}:10:git status\0");
        apply_wire(&mut reducer, &mut decoder, frame.as_bytes());

        let restore = reducer.route_input(b"\x12");
        assert!(restore.effects().effects().iter().any(|effect| {
            matches!(effect, SessionEffect::ReplaceBuffer(value) if value.as_str() == "git" && value.cursor() == 3)
        }));
        assert_eq!(reducer.mode(), SessionMode::Spec);
    }

    #[test]
    fn escape_unwinds_acknowledged_history_before_forwarding_and_requires_ack() {
        let (mut reducer, mut decoder) = ready();
        let _ = synchronize(&mut reducer, &mut decoder, b"git", "git", 3);
        let preview_nonce = request_history_preview(&mut reducer, "git", "git status");
        let frame = format!("probe-buffer:b:{preview_nonce}:10:git status\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(reducer.history_preview_active);

        let pending_escape = reducer.route_input(b"\x1b");
        assert!(pending_escape.effects().is_empty());
        let unwind = reducer.flush_input();
        let effects = unwind.effects().effects();
        let replacement = effects.iter().position(|effect| {
            matches!(effect, SessionEffect::ReplaceBuffer(value) if value.as_str() == "git" && value.cursor() == 3)
        });
        let escape = effects.iter().position(|effect| {
            matches!(effect, SessionEffect::ForwardInput(bytes) if bytes.as_ref() == b"\x1b")
        });
        let sync = effects
            .iter()
            .position(|effect| matches!(effect, SessionEffect::RequestBufferSync(_)));
        assert!(replacement.is_some_and(|replacement| {
            escape
                .is_some_and(|escape| sync.is_some_and(|sync| replacement < sync && sync < escape))
        }));
        assert_eq!(reducer.mode(), SessionMode::Spec);
        assert!(reducer.replacement_pending());
        assert!(reducer.active_query().is_none());

        let nonce = effects
            .iter()
            .find_map(|effect| match effect {
                SessionEffect::RequestBufferSync(nonce) => Some(nonce.get()),
                _ => None,
            })
            .unwrap();
        let frame = format!("probe-buffer:b:{nonce}:3:git\0");
        let _ = apply_wire(&mut reducer, &mut decoder, frame.as_bytes());
        assert!(!reducer.replacement_pending());
        assert_eq!(reducer.active_query().unwrap().line, "git");
        assert_eq!(reducer.active_query().unwrap().cursor, 3);
    }

    #[test]
    fn authority_effect_wrappers_are_not_clone() {
        trait AmbiguousIfClone<Marker> {
            fn marker() {}
        }
        struct CloneMarker;
        impl<T: ?Sized> AmbiguousIfClone<()> for T {}
        impl<T: Clone> AmbiguousIfClone<CloneMarker> for T {}

        let _ = <SessionEffect as AmbiguousIfClone<_>>::marker;
        let _ = <EffectBatch as AmbiguousIfClone<_>>::marker;
        let _ = <InputReduction as AmbiguousIfClone<_>>::marker;
        let _ = <PresentationReduction as AmbiguousIfClone<_>>::marker;
    }

    #[test]
    fn sensitive_input_and_replacements_are_redacted_from_debug() {
        let replacement = BufferReplacement::new("secret-token", 12).unwrap();
        let batch = EffectBatch {
            effects: vec![
                SessionEffect::ForwardInput(b"hunter2".to_vec().into_boxed_slice()),
                SessionEffect::ReplaceBuffer(replacement),
            ],
        };
        let debug = format!("{batch:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("byte_count"));
    }
}
