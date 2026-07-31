//! Cell-aware, non-destructive terminal overlay rendering.
//!
//! Rendering produces inert, bounded byte transactions. The caller owns the
//! single serialized write boundary shared with child output and must clear an
//! existing transaction before forwarding child bytes.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::completion::{CompletionQuery, Suggestion, SuggestionSource};
use crate::config::{Style, Ui};
use crate::screen::{CursorPosition, ScreenBuffer, ScreenSnapshot, TerminalSize};
use crate::selection::{SelectionState, ghost_suffix};

/// Default and maximum menu width in terminal cells.
pub const MAX_MENU_WIDTH: u16 = 76;
/// Minimum useful menu height.
pub const MIN_USABLE_MENU_HEIGHT: u16 = 3;
/// Maximum configured candidate rows accepted by the renderer.
pub const MAX_MENU_HEIGHT: u16 = 50;
/// Maximum bytes emitted by one serialized render transaction.
pub const MAX_RENDER_BYTES: usize = 32 * 1024;
/// Maximum source bytes inspected for any displayed candidate field.
pub const MAX_DISPLAY_SOURCE_BYTES: usize = 4 * 1024;
/// Maximum independently owned menu, footer, and ghost spans.
pub const MAX_OWNED_SPANS: usize = MAX_MENU_HEIGHT as usize + 2;

static NEXT_RENDERER_ID: AtomicU64 = AtomicU64::new(1);

/// Renderer settings resolved from the live UI configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayOptions {
    /// Modern or classic visual style.
    pub style: Style,
    /// Whether known Nerd Font markers may be used.
    pub nerd_fonts: bool,
    /// Whether a safe selected suffix may be shown inline.
    pub ghost_text: bool,
    /// Maximum candidate rows.
    pub max_height: u16,
    /// Minimum rows required before a menu is useful.
    pub min_usable_height: u16,
}

impl OverlayOptions {
    /// Resolves bounded render settings from validated UI configuration.
    #[must_use]
    pub const fn from_ui(ui: Ui) -> Self {
        Self {
            style: ui.style,
            nerd_fonts: ui.nerd_fonts,
            ghost_text: ui.ghost_text,
            max_height: clamp_u16(ui.max_height, MIN_USABLE_MENU_HEIGHT, MAX_MENU_HEIGHT),
            min_usable_height: MIN_USABLE_MENU_HEIGHT,
        }
    }

    const fn bounded(self) -> Self {
        Self {
            style: self.style,
            nerd_fonts: self.nerd_fonts,
            ghost_text: self.ghost_text,
            max_height: clamp_u16(self.max_height, MIN_USABLE_MENU_HEIGHT, MAX_MENU_HEIGHT),
            min_usable_height: clamp_u16(
                self.min_usable_height,
                MIN_USABLE_MENU_HEIGHT,
                MAX_MENU_HEIGHT,
            ),
        }
    }
}

const fn clamp_u16(value: u16, minimum: u16, maximum: u16) -> u16 {
    if value < minimum {
        minimum
    } else if value > maximum {
        maximum
    } else {
        value
    }
}

/// Borrowed, authority-checked state for one redraw.
#[derive(Clone, Copy)]
pub struct OverlayRequest<'a> {
    query: &'a CompletionQuery,
    selection: &'a SelectionState,
    footer_hint: &'a str,
}

impl<'a> OverlayRequest<'a> {
    /// Creates a request using the query and selection from the same authority.
    #[must_use]
    pub const fn new(query: &'a CompletionQuery, selection: &'a SelectionState) -> Self {
        Self {
            query,
            selection,
            footer_hint: "Tab insert  Esc hide",
        }
    }

    /// Sets a caller-resolved, sanitized-on-render shortcut hint.
    #[must_use]
    pub const fn with_footer_hint(mut self, footer_hint: &'a str) -> Self {
        self.footer_hint = footer_hint;
        self
    }
}

impl fmt::Debug for OverlayRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OverlayRequest")
            .field("query_generation", &self.query.generation)
            .field("query_bytes", &self.query.line.len())
            .field("candidate_count", &self.selection.candidates().len())
            .field("selected", &self.selection.selected_index())
            .field("footer_hint_bytes", &self.footer_hint.len())
            .finish()
    }
}

/// Purpose of one owned terminal span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnedSpanKind {
    /// Candidate menu row.
    Menu,
    /// Position and shortcut footer.
    Footer,
    /// Inline suffix after the shell cursor.
    Ghost,
}

/// Exact contiguous terminal cells owned by argmax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedSpan {
    /// Zero-based row.
    pub row: u16,
    /// Zero-based starting column.
    pub column: u16,
    /// Number of display cells.
    pub cells: u16,
    /// Semantic span purpose.
    pub kind: OwnedSpanKind,
}

/// Explicit region that must be cleared before shell output or redraw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedRegion {
    terminal_size: TerminalSize,
    spans: Box<[OwnedSpan]>,
    ghost_clearance: Option<u16>,
    restore_cursor: CursorPosition,
}

impl OwnedRegion {
    /// Terminal size for which coordinates were produced.
    #[must_use]
    pub const fn terminal_size(&self) -> TerminalSize {
        self.terminal_size
    }

    /// Bounded owned spans.
    #[must_use]
    pub const fn spans(&self) -> &[OwnedSpan] {
        &self.spans
    }
}

/// Safe visible part of the selected ghost suffix.
pub struct DisplayedGhost {
    accepted_suffix: Box<str>,
    cells: u16,
    clipped: bool,
}

impl DisplayedGhost {
    /// Exact suffix bytes represented before any visual ellipsis.
    #[must_use]
    pub const fn accepted_suffix(&self) -> &str {
        &self.accepted_suffix
    }

    /// Display cells occupied, including a clipping ellipsis.
    #[must_use]
    pub const fn cells(&self) -> u16 {
        self.cells
    }

    /// Whether the full selected suffix did not fit.
    #[must_use]
    pub const fn clipped(&self) -> bool {
        self.clipped
    }
}

impl fmt::Debug for DisplayedGhost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisplayedGhost")
            .field("accepted_bytes", &self.accepted_suffix.len())
            .field("cells", &self.cells)
            .field("clipped", &self.clipped)
            .finish()
    }
}

/// Purpose of a serialized renderer transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionKind {
    /// Cleared prior cells and drew a new overlay.
    Draw,
    /// Cleared prior cells without drawing a replacement.
    Clear,
    /// Drawing was unsafe; no terminal bytes were produced.
    Suppressed,
}

/// One indivisible terminal-write transaction.
///
/// After fully writing non-empty [`Self::bytes`], pass this transaction to
/// [`OverlayRenderer::acknowledge_transaction`].
pub struct RenderTransaction {
    renderer_id: NonZeroU64,
    sequence: u64,
    kind: TransactionKind,
    bytes: Box<[u8]>,
}

impl RenderTransaction {
    /// Monotonic renderer-local serialization token.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Draw, clear, or suppressed disposition.
    #[must_use]
    pub const fn kind(&self) -> TransactionKind {
        self.kind
    }

    /// Inert ANSI bytes to write atomically with respect to shell output.
    #[must_use]
    pub const fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether the transaction writes no terminal bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for RenderTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderTransaction")
            .field("sequence", &self.sequence)
            .field("kind", &self.kind)
            .field("byte_count", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Render transaction plus safe ghost acceptance metadata.
pub struct OverlayFrame {
    transaction: RenderTransaction,
    ghost: Option<DisplayedGhost>,
}

impl OverlayFrame {
    /// Serialized terminal output.
    #[must_use]
    pub const fn transaction(&self) -> &RenderTransaction {
        &self.transaction
    }

    /// Exact visible ghost suffix metadata, when present.
    #[must_use]
    pub const fn ghost(&self) -> Option<&DisplayedGhost> {
        self.ghost.as_ref()
    }

    /// Takes ownership of the transaction and ghost metadata.
    #[must_use]
    pub fn into_parts(self) -> (RenderTransaction, Option<DisplayedGhost>) {
        (self.transaction, self.ghost)
    }
}

impl fmt::Debug for OverlayFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OverlayFrame")
            .field("transaction", &self.transaction)
            .field("ghost", &self.ghost)
            .finish()
    }
}

/// Bounded rendering failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayError {
    /// Process-wide renderer identities were exhausted.
    RendererIdExhausted,
    /// Renderer-local transaction identifiers were exhausted.
    SequenceExhausted,
    /// A transaction exceeded [`MAX_RENDER_BYTES`] and was not emitted.
    OutputLimit,
    /// A renderer write must be acknowledged or recovered before continuing.
    TransactionPending {
        /// Sequence awaiting completion.
        sequence: u64,
    },
    /// An acknowledgment did not refer to the latest renderer transaction.
    UnexpectedTransaction {
        /// Latest renderer-local sequence.
        expected: u64,
        /// Sequence supplied by the caller.
        actual: u64,
    },
    /// A transaction was created by a different renderer.
    ForeignTransaction,
}

impl fmt::Display for OverlayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RendererIdExhausted => formatter.write_str("overlay renderer IDs are exhausted"),
            Self::SequenceExhausted => formatter.write_str("overlay sequence is exhausted"),
            Self::OutputLimit => formatter.write_str("overlay output exceeded its byte limit"),
            Self::TransactionPending { sequence } => {
                write!(formatter, "overlay transaction {sequence} is still pending")
            }
            Self::UnexpectedTransaction { expected, actual } => write!(
                formatter,
                "overlay transaction {actual} cannot acknowledge latest transaction {expected}"
            ),
            Self::ForeignTransaction => {
                formatter.write_str("overlay transaction belongs to a different renderer")
            }
        }
    }
}

impl Error for OverlayError {}

#[derive(Default)]
struct AnsiBuilder {
    bytes: Vec<u8>,
}

impl AnsiBuilder {
    fn push(&mut self, value: &str) -> Result<(), OverlayError> {
        if self.bytes.len().saturating_add(value.len()) > MAX_RENDER_BYTES {
            return Err(OverlayError::OutputLimit);
        }
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn erase_cells(&mut self, cells: u16) -> Result<(), OverlayError> {
        self.push(&format!("\x1b[{cells}X"))
    }

    fn cup(&mut self, row: u16, column: u16) -> Result<(), OverlayError> {
        self.push(&format!("\x1b[{};{}H", row + 1, column + 1))
    }

    fn finish(self) -> Box<[u8]> {
        self.bytes.into_boxed_slice()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MenuPlan {
    start_row: u16,
    start_column: u16,
    width: u16,
    item_rows: u16,
    footer: bool,
}

#[derive(Debug)]
struct GhostPlan {
    visual: String,
    accepted: String,
    cells: u16,
    clipped: bool,
}

#[derive(Clone, Debug)]
struct PendingTransaction {
    sequence: u64,
    recovery: OwnedRegion,
    next_owned: Option<OwnedRegion>,
}

/// Stateful renderer that owns prior cells and a stable candidate window.
#[derive(Default)]
pub struct OverlayRenderer {
    owned: Option<OwnedRegion>,
    renderer_id: Option<NonZeroU64>,
    sequence: u64,
    window_generation: Option<u64>,
    window_start: usize,
    pending: Option<PendingTransaction>,
}

impl fmt::Debug for OverlayRenderer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OverlayRenderer")
            .field(
                "owned_span_count",
                &self.owned.as_ref().map_or(0, |owned| owned.spans.len()),
            )
            .field("sequence", &self.sequence)
            .field("window_generation", &self.window_generation)
            .field("window_start", &self.window_start)
            .field(
                "pending_sequence",
                &self.pending.as_ref().map(|pending| pending.sequence),
            )
            .finish_non_exhaustive()
    }
}

impl OverlayRenderer {
    /// Creates an empty renderer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            owned: None,
            renderer_id: None,
            sequence: 0,
            window_generation: None,
            window_start: 0,
            pending: None,
        }
    }

    /// Region owned after the latest acknowledged transaction.
    #[must_use]
    pub const fn owned_region(&self) -> Option<&OwnedRegion> {
        self.owned.as_ref()
    }

    /// Acknowledges that every byte of the latest transaction was written.
    ///
    /// Callers must acknowledge a non-empty transaction before requesting
    /// another normal renderer operation. In particular, the transaction from
    /// [`Self::before_shell_output`] must be fully written and acknowledged
    /// before forwarding child output. On a partial or failed write, callers
    /// must use [`Self::on_failure`] instead.
    ///
    /// Empty transactions require no acknowledgment, but acknowledging the
    /// latest empty transaction is accepted for uniform caller logic.
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::ForeignTransaction`] when another renderer
    /// created `transaction`, or [`OverlayError::UnexpectedTransaction`] when
    /// it is not this renderer's latest transaction.
    pub fn acknowledge_transaction(
        &mut self,
        transaction: &RenderTransaction,
    ) -> Result<(), OverlayError> {
        if self.renderer_id != Some(transaction.renderer_id) {
            return Err(OverlayError::ForeignTransaction);
        }
        let expected = self
            .pending
            .as_ref()
            .map_or(self.sequence, |pending| pending.sequence);
        if transaction.sequence != expected {
            return Err(OverlayError::UnexpectedTransaction {
                expected,
                actual: transaction.sequence,
            });
        }
        if let Some(pending) = self.pending.take() {
            self.owned = pending.next_owned;
        }
        Ok(())
    }

    /// Clears prior cells and draws a menu and/or ghost when safe.
    ///
    /// # Errors
    ///
    /// Returns a bounded error without emitting partial bytes.
    pub fn render(
        &mut self,
        screen: ScreenSnapshot,
        request: OverlayRequest<'_>,
        options: OverlayOptions,
    ) -> Result<OverlayFrame, OverlayError> {
        self.ensure_transaction_complete()?;
        let options = options.bounded();
        if !screen.overlay_safe()
            || !request.selection.is_visible()
            || request.query.generation != request.selection.generation()
        {
            return self.suppress_or_clear(screen);
        }

        let candidates = request.selection.candidates();
        let selected = request.selection.selected_index();
        let menu_area_clear = screen.rows_below_clear() || self.can_reuse_menu_area(screen);
        let menu =
            selected.and_then(|_| menu_plan(screen, candidates.len(), options, menu_area_clear));
        let ghost_clearance = self.ghost_clearance(screen);
        let ghost = if options.ghost_text {
            selected
                .and_then(|index| candidates.get(index))
                .and_then(|candidate| ghost_plan(request.query, candidate, ghost_clearance))
        } else {
            None
        };

        if menu.is_none() && ghost.is_none() {
            return self.clear(screen);
        }

        let (renderer_id, sequence) = self.next_transaction()?;
        let mut builder = AnsiBuilder::default();
        let mut recovery_spans = self
            .owned
            .as_ref()
            .map_or_else(Vec::new, |owned| owned.spans.to_vec());
        self.clear_owned_spans(&mut builder, screen.size())?;

        let mut spans = Vec::with_capacity(MAX_OWNED_SPANS);
        if let (Some(plan), Some(selected)) = (menu, selected) {
            self.draw_menu(&mut builder, &mut spans, plan, request, options, selected)?;
        }
        if let Some(ghost) = ghost.as_ref() {
            let cursor = screen.cursor();
            builder.cup(cursor.row, cursor.column)?;
            builder.push(&ghost.visual)?;
            spans.push(OwnedSpan {
                row: cursor.row,
                column: cursor.column,
                cells: ghost.cells,
                kind: OwnedSpanKind::Ghost,
            });
        }
        builder.cup(screen.cursor().row, screen.cursor().column)?;

        debug_assert!(spans.len() <= MAX_OWNED_SPANS);
        recovery_spans.extend(spans.iter().copied());
        let recovery = OwnedRegion {
            terminal_size: screen.size(),
            spans: recovery_spans.into_boxed_slice(),
            ghost_clearance: None,
            restore_cursor: screen.cursor(),
        };
        let next_owned = OwnedRegion {
            terminal_size: screen.size(),
            spans: spans.into_boxed_slice(),
            ghost_clearance: ghost.as_ref().map(|_| ghost_clearance),
            restore_cursor: screen.cursor(),
        };
        self.pending = Some(PendingTransaction {
            sequence,
            recovery,
            next_owned: Some(next_owned),
        });
        Ok(OverlayFrame {
            transaction: RenderTransaction {
                renderer_id,
                sequence,
                kind: TransactionKind::Draw,
                bytes: builder.finish(),
            },
            ghost: ghost.map(|ghost| DisplayedGhost {
                accepted_suffix: ghost.accepted.into_boxed_str(),
                cells: ghost.cells,
                clipped: ghost.clipped,
            }),
        })
    }

    /// Clears every prior owned cell before hiding the overlay.
    ///
    /// # Errors
    ///
    /// Returns a bounded error without emitting partial bytes.
    pub fn clear(&mut self, screen: ScreenSnapshot) -> Result<OverlayFrame, OverlayError> {
        self.ensure_transaction_complete()?;
        let (renderer_id, sequence) = self.next_transaction()?;
        let Some(mut recovery) = self.owned.clone() else {
            return Ok(empty_frame(renderer_id, sequence, TransactionKind::Clear));
        };
        if !can_address(screen) {
            return Ok(empty_frame(
                renderer_id,
                sequence,
                TransactionKind::Suppressed,
            ));
        }

        let mut builder = AnsiBuilder::default();
        self.clear_owned_spans(&mut builder, screen.size())?;
        builder.cup(screen.cursor().row, screen.cursor().column)?;
        recovery.terminal_size = screen.size();
        recovery.restore_cursor = screen.cursor();
        self.pending = Some(PendingTransaction {
            sequence,
            recovery,
            next_owned: None,
        });
        Ok(OverlayFrame {
            transaction: RenderTransaction {
                renderer_id,
                sequence,
                kind: TransactionKind::Clear,
                bytes: builder.finish(),
            },
            ghost: None,
        })
    }

    /// Alias for clearing before forwarding serialized shell output.
    ///
    /// # Errors
    ///
    /// Returns the same bounded failures as [`Self::clear`].
    ///
    /// A non-empty returned transaction must be written in full and passed to
    /// [`Self::acknowledge_transaction`] before any child bytes are forwarded.
    pub fn before_shell_output(
        &mut self,
        screen: ScreenSnapshot,
    ) -> Result<OverlayFrame, OverlayError> {
        self.clear(screen)
    }

    /// Clears the intersecting old region after a terminal resize.
    ///
    /// # Errors
    ///
    /// Returns the same bounded failures as [`Self::clear`].
    pub fn on_resize(&mut self, screen: ScreenSnapshot) -> Result<OverlayFrame, OverlayError> {
        self.clear(screen)
    }

    /// Clears prior cells on renderer or session failure.
    ///
    /// This may replace an unacknowledged transaction after a partial write.
    /// A non-empty cleanup transaction also requires acknowledgment after it is
    /// written successfully.
    ///
    /// # Errors
    ///
    /// Returns a bounded error without emitting partial bytes.
    pub fn on_failure(&mut self, screen: ScreenSnapshot) -> Result<OverlayFrame, OverlayError> {
        let (renderer_id, sequence) = self.next_transaction()?;
        let recovery = self
            .pending
            .as_ref()
            .map(|pending| &pending.recovery)
            .filter(|recovery| recovery.terminal_size == screen.size())
            .or_else(|| {
                self.owned
                    .as_ref()
                    .filter(|owned| owned.terminal_size == screen.size())
            });
        let Some(recovery) = recovery.cloned() else {
            self.pending = None;
            self.owned = None;
            return Ok(empty_frame(renderer_id, sequence, TransactionKind::Clear));
        };
        if !can_recover(screen) {
            self.pending = None;
            self.owned = None;
            return Ok(empty_frame(
                renderer_id,
                sequence,
                TransactionKind::Suppressed,
            ));
        }

        let mut builder = AnsiBuilder::default();
        clear_spans(&mut builder, recovery.spans(), screen.size())?;
        builder.cup(recovery.restore_cursor.row, recovery.restore_cursor.column)?;
        self.pending = Some(PendingTransaction {
            sequence,
            recovery,
            next_owned: None,
        });
        Ok(OverlayFrame {
            transaction: RenderTransaction {
                renderer_id,
                sequence,
                kind: TransactionKind::Clear,
                bytes: builder.finish(),
            },
            ghost: None,
        })
    }

    fn suppress_or_clear(&mut self, screen: ScreenSnapshot) -> Result<OverlayFrame, OverlayError> {
        if can_address(screen) {
            return self.clear(screen);
        }
        let (renderer_id, sequence) = self.next_transaction()?;
        Ok(empty_frame(
            renderer_id,
            sequence,
            TransactionKind::Suppressed,
        ))
    }

    fn next_transaction(&mut self) -> Result<(NonZeroU64, u64), OverlayError> {
        let next = self
            .sequence
            .checked_add(1)
            .ok_or(OverlayError::SequenceExhausted)?;
        let renderer_id = if let Some(renderer_id) = self.renderer_id {
            renderer_id
        } else {
            let renderer_id = allocate_renderer_id(&NEXT_RENDERER_ID)?;
            self.renderer_id = Some(renderer_id);
            renderer_id
        };
        self.sequence = next;
        Ok((renderer_id, next))
    }

    fn ensure_transaction_complete(&self) -> Result<(), OverlayError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(());
        };
        Err(OverlayError::TransactionPending {
            sequence: pending.sequence,
        })
    }

    fn clear_owned_spans(
        &self,
        builder: &mut AnsiBuilder,
        current_size: TerminalSize,
    ) -> Result<(), OverlayError> {
        let Some(owned) = self.owned.as_ref() else {
            return Ok(());
        };
        clear_spans(builder, owned.spans(), current_size)
    }

    fn can_reuse_menu_area(&self, screen: ScreenSnapshot) -> bool {
        self.owned.as_ref().is_some_and(|owned| {
            owned.terminal_size == screen.size()
                && owned.spans.iter().any(|span| {
                    span.kind == OwnedSpanKind::Menu
                        && span.row == screen.cursor().row.saturating_add(1)
                })
        })
    }

    fn ghost_clearance(&self, screen: ScreenSnapshot) -> u16 {
        let tracked = self
            .owned
            .as_ref()
            .filter(|owned| owned.terminal_size == screen.size())
            .and_then(|owned| {
                owned
                    .spans
                    .iter()
                    .any(|span| {
                        span.kind == OwnedSpanKind::Ghost
                            && span.row == screen.cursor().row
                            && span.column == screen.cursor().column
                    })
                    .then_some(owned.ghost_clearance)
                    .flatten()
            })
            .unwrap_or_else(|| screen.blank_cells_to_right());
        let before_last_column = screen
            .size()
            .columns()
            .saturating_sub(screen.cursor().column)
            .saturating_sub(1);
        tracked.min(before_last_column)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_menu(
        &mut self,
        builder: &mut AnsiBuilder,
        spans: &mut Vec<OwnedSpan>,
        plan: MenuPlan,
        request: OverlayRequest<'_>,
        options: OverlayOptions,
        selected: usize,
    ) -> Result<(), OverlayError> {
        let candidates = request.selection.candidates();
        let capacity = usize::from(plan.item_rows);
        let start = self.stable_window(
            request.selection.generation(),
            candidates.len(),
            selected,
            capacity,
        );
        for (visible, candidate) in candidates.iter().skip(start).take(capacity).enumerate() {
            let index = start + visible;
            let row = plan.start_row + u16::try_from(visible).unwrap_or(0);
            let text = menu_row(candidate, index == selected, plan.width, options);
            builder.cup(row, plan.start_column)?;
            builder.push(&text)?;
            spans.push(OwnedSpan {
                row,
                column: plan.start_column,
                cells: plan.width,
                kind: OwnedSpanKind::Menu,
            });
        }

        if plan.footer {
            let row = plan.start_row + plan.item_rows;
            let footer = footer_row(
                selected,
                candidates.len(),
                capacity,
                request.footer_hint,
                plan.width,
                options.style,
            );
            builder.cup(row, plan.start_column)?;
            builder.push(&footer)?;
            spans.push(OwnedSpan {
                row,
                column: plan.start_column,
                cells: plan.width,
                kind: OwnedSpanKind::Footer,
            });
        }
        Ok(())
    }

    fn stable_window(
        &mut self,
        generation: u64,
        total: usize,
        selected: usize,
        capacity: usize,
    ) -> usize {
        if self.window_generation != Some(generation) {
            self.window_generation = Some(generation);
            self.window_start = 0;
        }
        let capacity = capacity.max(1);
        if selected < self.window_start {
            self.window_start = selected;
        } else if selected >= self.window_start.saturating_add(capacity) {
            self.window_start = selected + 1 - capacity;
        }
        self.window_start = self.window_start.min(total.saturating_sub(capacity));
        self.window_start
    }
}

fn clear_spans(
    builder: &mut AnsiBuilder,
    spans: &[OwnedSpan],
    current_size: TerminalSize,
) -> Result<(), OverlayError> {
    for span in spans {
        if span.row >= current_size.rows() || span.column >= current_size.columns() {
            continue;
        }
        let cells = span
            .cells
            .min(current_size.columns().saturating_sub(span.column));
        if cells == 0 {
            continue;
        }
        builder.cup(span.row, span.column)?;
        builder.erase_cells(cells)?;
    }
    Ok(())
}

fn allocate_renderer_id(counter: &AtomicU64) -> Result<NonZeroU64, OverlayError> {
    let renderer_id = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| OverlayError::RendererIdExhausted)?;
    NonZeroU64::new(renderer_id).ok_or(OverlayError::RendererIdExhausted)
}

fn empty_frame(renderer_id: NonZeroU64, sequence: u64, kind: TransactionKind) -> OverlayFrame {
    OverlayFrame {
        transaction: RenderTransaction {
            renderer_id,
            sequence,
            kind,
            bytes: Box::default(),
        },
        ghost: None,
    }
}

fn can_address(screen: ScreenSnapshot) -> bool {
    screen.synchronized() && can_recover(screen)
}

fn can_recover(screen: ScreenSnapshot) -> bool {
    // A failed renderer transaction contains only printable UTF-8, CUP, and
    // ECH. A fresh ESC restarts any incomplete prefix of those sequences.
    // Static modes still have to make absolute addressing and overwrite safe.
    matches!(screen.buffer(), ScreenBuffer::Primary)
        && screen.scroll_region().is_full(screen.size())
        && !screen.origin_mode()
        && !screen.insert_mode()
        && !screen.wrap_pending()
}

fn menu_plan(
    screen: ScreenSnapshot,
    total: usize,
    options: OverlayOptions,
    menu_area_clear: bool,
) -> Option<MenuPlan> {
    if total == 0 || !menu_area_clear || !screen.scroll_region().is_full(screen.size()) {
        return None;
    }
    let size = screen.size();
    if size.rows() <= options.min_usable_height || size.columns() < 8 {
        return None;
    }
    let desired_items = total.min(usize::from(options.max_height));
    let footer = matches!(options.style, Style::Modern) || total > desired_items;
    let desired_rows = u16::try_from(desired_items)
        .unwrap_or(options.max_height)
        .saturating_add(u16::from(footer));
    let below = size.rows() - screen.cursor().row - 1;
    let available = below.min(desired_rows);
    let minimum = options.min_usable_height.min(desired_rows);
    if available < minimum {
        return None;
    }
    let footer = footer && available >= 2;
    let item_rows = available
        .saturating_sub(u16::from(footer))
        .min(u16::try_from(desired_items).unwrap_or(options.max_height));
    if item_rows == 0 {
        return None;
    }
    let width = MAX_MENU_WIDTH.min(size.columns().saturating_sub(1));
    if width == 0 {
        return None;
    }
    let start_column = screen
        .cursor()
        .column
        .min(size.columns().saturating_sub(width).saturating_sub(1));
    Some(MenuPlan {
        start_row: screen.cursor().row + 1,
        start_column,
        width,
        item_rows,
        footer,
    })
}

fn ghost_plan(
    query: &CompletionQuery,
    candidate: &Suggestion,
    available: u16,
) -> Option<GhostPlan> {
    if query.cursor != query.line.len()
        || query.line.len() > MAX_DISPLAY_SOURCE_BYTES
        || candidate.edit().replacement.len() > MAX_DISPLAY_SOURCE_BYTES
    {
        return None;
    }
    let result = candidate.resulting_line(query).ok()?;
    let suffix = ghost_suffix(&query.line, &result)?;
    let starts_at_grapheme_boundary = UnicodeSegmentation::grapheme_indices(result.as_str(), true)
        .any(|(index, _)| index == query.line.len());
    let first = suffix.chars().next()?;
    if suffix.len() > MAX_DISPLAY_SOURCE_BYTES
        || suffix.chars().any(is_unsafe_display_character)
        || !starts_at_grapheme_boundary
        || UnicodeWidthChar::width(first).unwrap_or(0) == 0
        || matches!(first, '\u{1F1E6}'..='\u{1F1FF}' | '\u{1F3FB}'..='\u{1F3FF}')
    {
        return None;
    }
    if available < 2 {
        return None;
    }
    clip_exact_ghost(suffix, available)
}

fn clip_exact_ghost(value: &str, available: u16) -> Option<GhostPlan> {
    let width = UnicodeWidthStr::width(value);
    if width == 0 {
        return None;
    }
    if width <= usize::from(available) {
        return Some(GhostPlan {
            visual: value.to_owned(),
            accepted: value.to_owned(),
            cells: u16::try_from(width).ok()?,
            clipped: false,
        });
    }

    let limit = usize::from(available.saturating_sub(1));
    let mut accepted = String::new();
    let mut cells = 0_usize;
    for grapheme in UnicodeSegmentation::graphemes(value, true) {
        let width = UnicodeWidthStr::width(grapheme);
        if cells.saturating_add(width) > limit {
            break;
        }
        accepted.push_str(grapheme);
        cells += width;
    }
    if accepted.is_empty() {
        return None;
    }
    let mut visual = accepted.clone();
    visual.push('…');
    Some(GhostPlan {
        visual,
        accepted,
        cells: u16::try_from(cells + 1).ok()?,
        clipped: true,
    })
}

fn menu_row(candidate: &Suggestion, selected: bool, width: u16, options: OverlayOptions) -> String {
    let marker = if selected { ">" } else { " " };
    let icon = icon_marker(candidate.icon(), candidate.source(), options.nerd_fonts);
    let badge = candidate.source().badge();
    let prefix = match options.style {
        Style::Modern => format!("{marker} {icon} [{badge}] "),
        Style::Classic => format!("{marker} [{badge}] "),
    };
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let available = usize::from(width).saturating_sub(prefix_width);
    let mut row = prefix;

    if matches!(options.style, Style::Modern)
        && !candidate.description().is_empty()
        && available >= 12
    {
        let separator = " — ";
        let separator_width = UnicodeWidthStr::width(separator);
        let description_width = (available / 3).max(4);
        let command_width = available.saturating_sub(separator_width + description_width);
        row.push_str(&clip_text(candidate.display(), command_width));
        row.push_str(separator);
        row.push_str(&clip_text(candidate.description(), description_width));
    } else {
        row.push_str(&clip_text(candidate.display(), available));
    }
    pad_or_clip(&row, usize::from(width))
}

fn footer_row(
    selected: usize,
    total: usize,
    capacity: usize,
    hint: &str,
    width: u16,
    style: Style,
) -> String {
    let position = format!("{}/{}", selected.saturating_add(1), total);
    let content = match style {
        Style::Modern => {
            let hint = sanitize_bounded(hint).0;
            if total > capacity {
                format!(" {position}  ↑/↓ navigate  {hint}")
            } else {
                format!(" {position}  {hint}")
            }
        }
        Style::Classic => format!(" {position}"),
    };
    pad_or_clip(&content, usize::from(width))
}

fn icon_marker(icon: &str, source: SuggestionSource, nerd_fonts: bool) -> &'static str {
    let icon = if icon.len() <= 16 { icon } else { "" };
    if nerd_fonts {
        if icon.eq_ignore_ascii_case("command") || icon.eq_ignore_ascii_case("terminal") {
            return "󰆍";
        }
        if icon.eq_ignore_ascii_case("file") {
            return "󰈔";
        }
        if icon.eq_ignore_ascii_case("directory") || icon.eq_ignore_ascii_case("folder") {
            return "󰉋";
        }
        if icon.eq_ignore_ascii_case("git") || icon.eq_ignore_ascii_case("branch") {
            return "󰘬";
        }
        if icon.eq_ignore_ascii_case("history") {
            return "󰋚";
        }
        return match source {
            SuggestionSource::File => "󰈔",
            SuggestionSource::History => "󰋚",
            _ => "•",
        };
    }
    if icon.eq_ignore_ascii_case("file") {
        return "f";
    }
    if icon.eq_ignore_ascii_case("directory") || icon.eq_ignore_ascii_case("folder") {
        return "d";
    }
    if icon.eq_ignore_ascii_case("git") || icon.eq_ignore_ascii_case("branch") {
        return "g";
    }
    if icon.eq_ignore_ascii_case("history") {
        return "h";
    }
    if icon.eq_ignore_ascii_case("command") || icon.eq_ignore_ascii_case("terminal") {
        return "c";
    }
    match source {
        SuggestionSource::File => "f",
        SuggestionSource::History => "h",
        _ => "?",
    }
}

fn pad_or_clip(value: &str, cells: usize) -> String {
    let mut value = clip_text(value, cells);
    let width = UnicodeWidthStr::width(value.as_str());
    value.extend(std::iter::repeat_n(' ', cells.saturating_sub(width)));
    value
}

fn clip_text(value: &str, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let (sanitized, source_truncated) = sanitize_bounded(value);
    let width = UnicodeWidthStr::width(sanitized.as_str());
    if width <= cells && !source_truncated {
        return sanitized;
    }

    let content_limit = cells.saturating_sub(1);
    let mut output = String::new();
    let mut width = 0_usize;
    for grapheme in UnicodeSegmentation::graphemes(sanitized.as_str(), true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width.saturating_add(grapheme_width) > content_limit {
            break;
        }
        output.push_str(grapheme);
        width += grapheme_width;
    }
    output.push('…');
    output
}

fn sanitize_bounded(value: &str) -> (String, bool) {
    let mut end = value.len().min(MAX_DISPLAY_SOURCE_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end != value.len();
    let mut sanitized = String::with_capacity(end.min(256));
    for character in value[..end].chars() {
        if is_unsafe_display_character(character) {
            sanitized.push('�');
        } else {
            sanitized.push(character);
        }
    }
    (sanitized, truncated)
}

fn is_unsafe_display_character(character: char) -> bool {
    character.is_control() || crate::ai::is_invisible(character)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::completion::{InsertionBehavior, SuggestionSource, TextEdit};
    use crate::screen::{ScreenObserver, TerminalSize};

    use super::*;

    fn query(line: &str) -> CompletionQuery {
        CompletionQuery {
            generation: 7,
            line: line.to_owned(),
            cursor: line.len(),
            cwd: PathBuf::from("/tmp"),
        }
    }

    fn suggestion(display: &str, description: &str, icon: &str) -> Suggestion {
        Suggestion::new(
            TextEdit {
                range: 0..3,
                replacement: display.to_owned(),
            },
            display,
            description,
            icon,
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            display,
        )
    }

    fn selection(candidates: Vec<Suggestion>, selected: usize) -> SelectionState {
        let mut state = SelectionState::default();
        state.begin_query(7, candidates);
        for _ in 0..selected {
            state.down();
        }
        state
    }

    fn ui(style: Style) -> OverlayOptions {
        OverlayOptions {
            style,
            nerd_fonts: false,
            ghost_text: true,
            max_height: 4,
            min_usable_height: 3,
        }
    }

    fn screen(columns: u16, rows: u16, output: &[u8]) -> ScreenObserver {
        let mut screen = ScreenObserver::new(TerminalSize::new(columns, rows).unwrap());
        let _ = screen.observe(output);
        screen
    }

    fn apply_frame(
        renderer: &mut OverlayRenderer,
        screen: &mut ScreenObserver,
        frame: &OverlayFrame,
    ) {
        let _ = screen.observe(frame.transaction().bytes());
        renderer
            .acknowledge_transaction(frame.transaction())
            .unwrap();
    }

    #[test]
    fn bottom_right_draw_suppresses_without_scrolling_or_mode_changes() {
        let mut screen = screen(24, 5, b"\x1b[5;24H");
        let before = screen.snapshot().cursor();
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
                suggestion("git stash", "save changes", "git"),
            ],
            0,
        );
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert!(frame.transaction().bytes().len() <= MAX_RENDER_BYTES);
        assert!(frame.transaction().is_empty());
        let _ = screen.observe(frame.transaction().bytes());
        assert_eq!(screen.snapshot().cursor(), before);
        assert!(screen.snapshot().wrapping());
        assert!(renderer.owned_region().is_none());
    }

    #[test]
    fn multiline_wide_combining_prompt_keeps_cell_anchor() {
        let mut screen = screen(32, 8, "Greendale 界\u{301}\r\n> git".as_bytes());
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "状态", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        let _ = screen.observe(frame.transaction().bytes());
        assert_eq!(screen.snapshot().cursor(), CursorPosition::new(1, 5));
        assert!(screen.row_text(2).unwrap().contains("> [spec] git status"));
    }

    #[test]
    fn right_prompt_and_occupied_lower_rows_are_never_overwritten() {
        let mut screen = screen(30, 6, b"> git\x1b7\x1b[1;20HRP\x1b[3;1Hold output\x1b8");
        let query = query("git");
        let selection = selection(
            vec![suggestion("git checkout a-very-long-branch", "", "git")],
            0,
        );
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();

        apply_frame(&mut renderer, &mut screen, &frame);
        assert!(renderer.owned_region().is_some_and(|owned| {
            owned
                .spans()
                .iter()
                .all(|span| span.kind == OwnedSpanKind::Ghost)
        }));
        assert!(frame.ghost().is_some_and(DisplayedGhost::clipped));
        assert!(screen.row_text(0).unwrap().ends_with("RP"));
        assert_eq!(screen.row_text(2).unwrap(), "old output");
    }

    #[test]
    fn rendered_transaction_is_chunk_partition_invariant() {
        let base = "Greendale 界\u{301}\r\n> git".as_bytes();
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let source = screen(32, 6, base);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let bytes = frame.transaction().bytes();

        let mut whole = screen(32, 6, base);
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..6)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();
        for split in 0..=bytes.len() {
            let mut chunked = screen(32, 6, base);
            let _ = chunked.observe(&bytes[..split]);
            let _ = chunked.observe(&bytes[split..]);
            assert_eq!(chunked.snapshot(), want, "split {split}");
            assert_eq!(
                (0..6)
                    .map(|row| chunked.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn clear_erases_owned_cells_and_allows_a_later_menu() {
        let mut screen = screen(30, 6, b"\x1b[3;1H> git");
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut renderer, &mut screen, &frame);

        let clear = renderer.clear(screen.snapshot()).unwrap();
        apply_frame(&mut renderer, &mut screen, &clear);
        assert!(screen.snapshot().rows_below_clear());

        let redraw = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert_eq!(redraw.transaction().kind(), TransactionKind::Draw);
        apply_frame(&mut renderer, &mut screen, &redraw);
        assert!(renderer.owned_region().is_some_and(|owned| {
            owned
                .spans()
                .iter()
                .any(|span| span.kind == OwnedSpanKind::Menu)
        }));
    }

    #[test]
    fn resize_and_shorter_ghost_clear_every_old_cell() {
        let mut screen = screen(40, 8, b"> git");
        let query = query("git");
        let mut renderer = OverlayRenderer::new();
        let long = selection(vec![suggestion("git checkout feature", "", "git")], 0);
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &long),
                ui(Style::Classic),
            )
            .unwrap();
        let long_cells = frame.ghost().unwrap().cells();
        apply_frame(&mut renderer, &mut screen, &frame);

        let short = selection(vec![suggestion("git st", "", "git")], 0);
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &short),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.ghost().unwrap().cells() < long_cells);
        apply_frame(&mut renderer, &mut screen, &frame);
        assert!(!screen.row_text(0).unwrap().contains("checkout"));

        let _ = screen.resize(TerminalSize::new(18, 5).unwrap());
        let clear = renderer.on_resize(screen.snapshot()).unwrap();
        apply_frame(&mut renderer, &mut screen, &clear);
        assert!(renderer.owned_region().is_none());
        assert!(clear.transaction().bytes().len() <= MAX_RENDER_BYTES);

        let _ = screen.observe(b"\x1b[2;1HUSER-CONTENT");
        let before_failure = screen.row_text(1);
        let failure = renderer.on_failure(screen.snapshot()).unwrap();
        assert!(failure.transaction().is_empty());
        apply_frame(&mut renderer, &mut screen, &failure);
        assert_eq!(screen.row_text(1), before_failure);
    }

    #[test]
    fn alternate_screen_and_tiny_terminal_suppress_menu() {
        let mut alternate = screen(24, 5, b"\x1b[?1049h");
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                alternate.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert_eq!(frame.transaction().kind(), TransactionKind::Suppressed);
        assert!(frame.transaction().is_empty());
        let _ = alternate.observe(b"\x1b[?1049l");

        let mut tiny = screen(8, 2, b"> git");
        let frame = renderer
            .render(
                tiny.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut renderer, &mut tiny, &frame);
        assert!(renderer.owned_region().is_some_and(|owned| {
            owned
                .spans()
                .iter()
                .all(|span| span.kind == OwnedSpanKind::Ghost)
        }));
        assert!(frame.ghost().is_some());
    }

    #[test]
    fn stale_query_generation_never_draws_candidates() {
        let screen = screen(30, 6, b"> git");
        let mut query = query("git");
        query.generation += 1;
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert_ne!(frame.transaction().kind(), TransactionKind::Draw);
        assert!(frame.transaction().is_empty());
        assert!(renderer.owned_region().is_none());
    }

    #[test]
    fn candidate_controls_and_hostile_icons_never_reach_ansi_output() {
        let screen = screen(76, 10, b"> git");
        let query = query("git");
        let candidate = Suggestion::new(
            TextEdit {
                range: 0..3,
                replacement: "git status".to_owned(),
            },
            "git\x1b]52;c;payload\x07status",
            "line\n\x1b[2Jdescription",
            "\x1b[31m",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "hostile",
        );
        let selection = selection(vec![candidate], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let bytes = frame.transaction().bytes();
        assert!(!bytes.windows(5).any(|window| window == b"\x1b]52"));
        assert!(!bytes.windows(4).any(|window| window == b"\x1b[2J"));
        assert!(std::str::from_utf8(bytes).unwrap().contains("[spec]"));
    }

    #[test]
    fn unsafe_terminal_modes_suppress_all_overlay_output() {
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        for output in [b"\x1b[3;6r\x1b[?6h".as_slice(), b"\x1b[4h".as_slice()] {
            let screen = screen(30, 6, output);
            let mut renderer = OverlayRenderer::new();
            let frame = renderer
                .render(
                    screen.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            assert_eq!(frame.transaction().kind(), TransactionKind::Suppressed);
            assert!(frame.transaction().is_empty());
            assert!(renderer.owned_region().is_none());
        }
    }

    #[test]
    fn incomplete_utf8_and_grapheme_tails_never_emit_overlay_bytes() {
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let outputs = [
            b"> \xF0\x9F".as_slice(),
            "> 👨‍".as_bytes(),
            "> ❤️".as_bytes(),
            "> 👋🏽".as_bytes(),
            "> 🇨".as_bytes(),
        ];

        for output in outputs {
            let screen = screen(30, 6, output);
            assert!(!screen.snapshot().overlay_safe());
            let mut renderer = OverlayRenderer::new();
            let frame = renderer
                .render(
                    screen.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            assert_eq!(frame.transaction().kind(), TransactionKind::Suppressed);
            assert!(frame.transaction().is_empty());
            assert!(renderer.owned_region().is_none());
        }

        for output in ["> 👨‍💻".as_bytes(), "> 🇨🇦".as_bytes()] {
            let screen = screen(30, 6, output);
            assert!(screen.snapshot().overlay_safe());
            let mut renderer = OverlayRenderer::new();
            let frame = renderer
                .render(
                    screen.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            assert_eq!(frame.transaction().kind(), TransactionKind::Draw);
            assert!(!frame.transaction().is_empty());
        }
    }

    #[test]
    fn complete_draw_preserves_the_child_saved_cursor() {
        let mut screen = screen(30, 8, b"\x1b[6;12H\x1b7\x1b[1;1H> git");
        let saved = screen.snapshot().saved_cursor();
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(
            !frame
                .transaction()
                .bytes()
                .windows(2)
                .any(|bytes| bytes == b"\x1b7")
        );
        assert!(
            !frame
                .transaction()
                .bytes()
                .windows(2)
                .any(|bytes| bytes == b"\x1b8")
        );
        apply_frame(&mut renderer, &mut screen, &frame);
        assert_eq!(screen.snapshot().saved_cursor(), saved);
        let _ = screen.observe(b"\x1b8");
        assert_eq!(screen.snapshot().cursor(), saved);
    }

    #[test]
    fn draw_replay_is_idempotent_and_does_not_scroll() {
        let base = b"\x1b[2;1H> git";
        let mut screen = screen(30, 8, base);
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert!(
            !frame
                .transaction()
                .bytes()
                .windows(2)
                .any(|bytes| bytes == b"\r\n")
        );
        let _ = screen.observe(frame.transaction().bytes());
        let once = screen.snapshot();
        let once_rows = (0..8)
            .map(|row| screen.row_text(row).unwrap())
            .collect::<Vec<_>>();
        let _ = screen.observe(frame.transaction().bytes());
        assert_eq!(screen.snapshot(), once);
        assert_eq!(
            (0..8)
                .map(|row| screen.row_text(row).unwrap())
                .collect::<Vec<_>>(),
            once_rows
        );
    }

    #[test]
    fn replay_recovers_every_partial_draw_prefix() {
        let base = b"\x1b[2;1H> git";
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let source = screen(30, 8, base);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let bytes = frame.transaction().bytes();
        let mut whole = screen(30, 8, base);
        let _ = whole.observe(bytes);
        let want = whole.snapshot();
        let want_rows = (0..8)
            .map(|row| whole.row_text(row).unwrap())
            .collect::<Vec<_>>();

        for split in 0..=bytes.len() {
            let mut replayed = screen(30, 8, base);
            let _ = replayed.observe(&bytes[..split]);
            let _ = replayed.observe(bytes);
            assert_eq!(replayed.snapshot(), want, "split {split}");
            assert_eq!(
                (0..8)
                    .map(|row| replayed.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                want_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn failure_cleanup_recovers_every_partial_draw_prefix() {
        let base = b"\x1b[2;1H> git";
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let source = screen(30, 8, base);
        let before = source.snapshot();
        let before_rows = (0..8)
            .map(|row| source.row_text(row).unwrap())
            .collect::<Vec<_>>();
        let mut renderer = OverlayRenderer::new();
        let bytes = renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap()
            .transaction()
            .bytes()
            .to_vec();

        for split in 0..=bytes.len() {
            let mut interrupted = screen(30, 8, base);
            let mut renderer = OverlayRenderer::new();
            let _ = renderer
                .render(
                    interrupted.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            let _ = interrupted.observe(&bytes[..split]);
            let cleanup = renderer.on_failure(interrupted.snapshot()).unwrap();
            assert_eq!(cleanup.transaction().kind(), TransactionKind::Clear);
            let _ = interrupted.observe(cleanup.transaction().bytes());
            assert_eq!(interrupted.snapshot(), before, "split {split}");
            assert_eq!(
                (0..8)
                    .map(|row| interrupted.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                before_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn acknowledged_shell_clear_never_erases_later_child_cells() {
        let mut screen = screen(30, 8, b"\x1b[2;1H> git");
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let mut renderer = OverlayRenderer::new();
        let draw = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut renderer, &mut screen, &draw);

        let clear = renderer.before_shell_output(screen.snapshot()).unwrap();
        apply_frame(&mut renderer, &mut screen, &clear);
        let _ = screen.observe(b"\x1b[3;1HUSER-CONTENT");
        let before = screen.snapshot();
        let before_rows = (0..8)
            .map(|row| screen.row_text(row).unwrap())
            .collect::<Vec<_>>();

        let failure = renderer.on_failure(screen.snapshot()).unwrap();
        assert_eq!(failure.transaction().kind(), TransactionKind::Clear);
        assert!(failure.transaction().is_empty());
        apply_frame(&mut renderer, &mut screen, &failure);
        assert_eq!(screen.snapshot(), before);
        assert_eq!(
            (0..8)
                .map(|row| screen.row_text(row).unwrap())
                .collect::<Vec<_>>(),
            before_rows
        );
        assert_eq!(screen.row_text(2).as_deref(), Some("USER-CONTENT"));
    }

    #[test]
    fn failure_cleanup_recovers_every_partial_shell_clear_prefix() {
        let base = b"\x1b[2;1H> git";
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let before = screen(30, 8, base);
        let before_snapshot = before.snapshot();
        let before_rows = (0..8)
            .map(|row| before.row_text(row).unwrap())
            .collect::<Vec<_>>();

        let mut source = screen(30, 8, base);
        let mut source_renderer = OverlayRenderer::new();
        let draw = source_renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut source_renderer, &mut source, &draw);
        let clear_bytes = source_renderer
            .before_shell_output(source.snapshot())
            .unwrap()
            .transaction()
            .bytes()
            .to_vec();

        for split in 0..=clear_bytes.len() {
            let mut interrupted = screen(30, 8, base);
            let mut renderer = OverlayRenderer::new();
            let draw = renderer
                .render(
                    interrupted.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            apply_frame(&mut renderer, &mut interrupted, &draw);
            let clear = renderer
                .before_shell_output(interrupted.snapshot())
                .unwrap();
            assert_eq!(clear.transaction().bytes(), clear_bytes);
            let _ = interrupted.observe(&clear.transaction().bytes()[..split]);

            let cleanup = renderer.on_failure(interrupted.snapshot()).unwrap();
            assert_eq!(cleanup.transaction().kind(), TransactionKind::Clear);
            apply_frame(&mut renderer, &mut interrupted, &cleanup);
            assert_eq!(interrupted.snapshot(), before_snapshot, "split {split}");
            assert_eq!(
                (0..8)
                    .map(|row| interrupted.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                before_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn failure_cleanup_retries_every_partial_cleanup_prefix() {
        let base = b"\x1b[2;1H> git";
        let query = query("git");
        let selection = selection(
            vec![
                suggestion("git status", "working tree", "git"),
                suggestion("git switch", "change branch", "git"),
            ],
            0,
        );
        let before = screen(30, 8, base);
        let before_snapshot = before.snapshot();
        let before_rows = (0..8)
            .map(|row| before.row_text(row).unwrap())
            .collect::<Vec<_>>();

        let mut source = screen(30, 8, base);
        let mut source_renderer = OverlayRenderer::new();
        let draw = source_renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let draw_prefix = draw.transaction().bytes().len() / 2;
        let _ = source.observe(&draw.transaction().bytes()[..draw_prefix]);
        let cleanup_bytes = source_renderer
            .on_failure(source.snapshot())
            .unwrap()
            .transaction()
            .bytes()
            .to_vec();

        for split in 0..=cleanup_bytes.len() {
            let mut interrupted = screen(30, 8, base);
            let mut renderer = OverlayRenderer::new();
            let draw = renderer
                .render(
                    interrupted.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            let _ = interrupted.observe(&draw.transaction().bytes()[..draw_prefix]);
            let cleanup = renderer.on_failure(interrupted.snapshot()).unwrap();
            assert_eq!(cleanup.transaction().bytes(), cleanup_bytes);
            let _ = interrupted.observe(&cleanup.transaction().bytes()[..split]);

            let retry = renderer.on_failure(interrupted.snapshot()).unwrap();
            assert_eq!(retry.transaction().kind(), TransactionKind::Clear);
            apply_frame(&mut renderer, &mut interrupted, &retry);
            assert_eq!(interrupted.snapshot(), before_snapshot, "split {split}");
            assert_eq!(
                (0..8)
                    .map(|row| interrupted.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                before_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn failure_cleanup_recovers_every_partial_resize_clear_prefix() {
        let base = b"\x1b[2;1H> git";
        let query = query("git");
        let selection = selection(vec![suggestion("git checkout feature", "", "git")], 0);
        let resized = TerminalSize::new(18, 5).unwrap();
        let mut before = screen(40, 8, base);
        let _ = before.resize(resized);
        let before_snapshot = before.snapshot();
        let before_rows = (0..5)
            .map(|row| before.row_text(row).unwrap())
            .collect::<Vec<_>>();

        let mut source = screen(40, 8, base);
        let mut source_renderer = OverlayRenderer::new();
        let draw = source_renderer
            .render(
                source.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut source_renderer, &mut source, &draw);
        let _ = source.resize(resized);
        let clear_bytes = source_renderer
            .on_resize(source.snapshot())
            .unwrap()
            .transaction()
            .bytes()
            .to_vec();

        for split in 0..=clear_bytes.len() {
            let mut interrupted = screen(40, 8, base);
            let mut renderer = OverlayRenderer::new();
            let draw = renderer
                .render(
                    interrupted.snapshot(),
                    OverlayRequest::new(&query, &selection),
                    ui(Style::Modern),
                )
                .unwrap();
            apply_frame(&mut renderer, &mut interrupted, &draw);
            let _ = interrupted.resize(resized);
            let clear = renderer.on_resize(interrupted.snapshot()).unwrap();
            assert_eq!(clear.transaction().bytes(), clear_bytes);
            let _ = interrupted.observe(&clear.transaction().bytes()[..split]);

            let cleanup = renderer.on_failure(interrupted.snapshot()).unwrap();
            assert_eq!(cleanup.transaction().kind(), TransactionKind::Clear);
            apply_frame(&mut renderer, &mut interrupted, &cleanup);
            assert_eq!(interrupted.snapshot(), before_snapshot, "split {split}");
            assert_eq!(
                (0..5)
                    .map(|row| interrupted.row_text(row).unwrap())
                    .collect::<Vec<_>>(),
                before_rows,
                "split {split}"
            );
        }
    }

    #[test]
    fn pending_transactions_require_matching_acknowledgments() {
        let screen = screen(30, 8, b"> git");
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let draw = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert_eq!(
            renderer.clear(screen.snapshot()).unwrap_err(),
            OverlayError::TransactionPending {
                sequence: draw.transaction().sequence()
            }
        );

        let cleanup = renderer.on_failure(screen.snapshot()).unwrap();
        assert_eq!(
            renderer
                .acknowledge_transaction(draw.transaction())
                .unwrap_err(),
            OverlayError::UnexpectedTransaction {
                expected: cleanup.transaction().sequence(),
                actual: draw.transaction().sequence(),
            }
        );
        renderer
            .acknowledge_transaction(cleanup.transaction())
            .unwrap();
    }

    #[test]
    fn foreign_same_sequence_draws_cannot_acknowledge() {
        let mut screen_a = screen(30, 8, b"> git");
        let mut screen_b = screen(30, 8, b"> git");
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer_a = OverlayRenderer::new();
        let mut renderer_b = OverlayRenderer::new();
        let draw_a = renderer_a
            .render(
                screen_a.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let draw_b = renderer_b
            .render(
                screen_b.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        assert_eq!(draw_a.transaction().sequence(), 1);
        assert_eq!(draw_b.transaction().sequence(), 1);
        let _ = screen_a.observe(draw_a.transaction().bytes());
        let _ = screen_b.observe(draw_b.transaction().bytes());

        assert_eq!(
            renderer_a
                .acknowledge_transaction(draw_b.transaction())
                .unwrap_err(),
            OverlayError::ForeignTransaction
        );
        assert!(renderer_a.owned_region().is_none());
        assert_eq!(
            renderer_a.clear(screen_a.snapshot()).unwrap_err(),
            OverlayError::TransactionPending { sequence: 1 }
        );
        renderer_a
            .acknowledge_transaction(draw_a.transaction())
            .unwrap();
        assert!(renderer_a.owned_region().is_some());
    }

    #[test]
    fn foreign_same_sequence_clears_cannot_acknowledge() {
        let mut screen_a = screen(30, 8, b"> git");
        let mut screen_b = screen(30, 8, b"> git");
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer_a = OverlayRenderer::new();
        let mut renderer_b = OverlayRenderer::new();
        let draw_a = renderer_a
            .render(
                screen_a.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut renderer_a, &mut screen_a, &draw_a);
        let draw_b = renderer_b
            .render(
                screen_b.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        apply_frame(&mut renderer_b, &mut screen_b, &draw_b);

        let clear_a = renderer_a.clear(screen_a.snapshot()).unwrap();
        let clear_b = renderer_b.clear(screen_b.snapshot()).unwrap();
        assert_eq!(clear_a.transaction().sequence(), 2);
        assert_eq!(clear_b.transaction().sequence(), 2);
        let _ = screen_a.observe(clear_a.transaction().bytes());
        let _ = screen_b.observe(clear_b.transaction().bytes());

        assert_eq!(
            renderer_a
                .acknowledge_transaction(clear_b.transaction())
                .unwrap_err(),
            OverlayError::ForeignTransaction
        );
        assert!(renderer_a.owned_region().is_some());
        renderer_a
            .acknowledge_transaction(clear_a.transaction())
            .unwrap();
        assert!(renderer_a.owned_region().is_none());
    }

    #[test]
    fn foreign_same_sequence_empty_transactions_cannot_acknowledge() {
        let screen = screen(30, 8, b"> git");
        let mut renderer_a = OverlayRenderer::new();
        let mut renderer_b = OverlayRenderer::new();
        let empty_a = renderer_a.clear(screen.snapshot()).unwrap();
        let empty_b = renderer_b.clear(screen.snapshot()).unwrap();
        assert_eq!(empty_a.transaction().sequence(), 1);
        assert_eq!(empty_b.transaction().sequence(), 1);
        assert!(empty_a.transaction().is_empty());
        assert!(empty_b.transaction().is_empty());

        assert_eq!(
            renderer_a
                .acknowledge_transaction(empty_b.transaction())
                .unwrap_err(),
            OverlayError::ForeignTransaction
        );
        renderer_a
            .acknowledge_transaction(empty_a.transaction())
            .unwrap();
    }

    #[test]
    fn renderer_id_allocation_fails_closed_at_exhaustion() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_renderer_id(&counter).unwrap().get(), u64::MAX - 1);
        assert_eq!(
            allocate_renderer_id(&counter),
            Err(OverlayError::RendererIdExhausted)
        );
    }

    #[test]
    fn failure_cleanup_uses_the_pre_draw_cursor_without_changing_wrap() {
        let mut screen = screen(30, 8, b"\x1b[?7l\x1b[2;1H> git");
        let before = screen.snapshot();
        assert!(!before.wrapping());
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let bytes = frame.transaction().bytes();
        let second_escape = bytes
            .windows(2)
            .enumerate()
            .filter(|(_, bytes)| *bytes == b"\x1b[")
            .nth(1)
            .map(|(index, _)| index)
            .unwrap();
        let _ = screen.observe(&bytes[..second_escape]);
        assert_ne!(screen.snapshot().cursor(), before.cursor());
        let cleanup = renderer.on_failure(screen.snapshot()).unwrap();
        let _ = screen.observe(cleanup.transaction().bytes());
        assert_eq!(screen.snapshot().cursor(), before.cursor());
        assert_eq!(screen.snapshot().wrapping(), before.wrapping());
    }

    #[test]
    fn leading_combining_ghost_is_rejected_without_touching_input() {
        let mut screen = screen(30, 6, b"> git");
        let before = screen.row_text(0).unwrap();
        let query = query("git");
        let selection = selection(vec![suggestion("git\u{301} status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.ghost().is_none());
        apply_frame(&mut renderer, &mut screen, &frame);
        let clear = renderer.clear(screen.snapshot()).unwrap();
        let _ = screen.observe(clear.transaction().bytes());
        assert_eq!(screen.row_text(0).as_deref(), Some(before.as_str()));
    }

    #[test]
    fn leading_emoji_modifier_ghost_cannot_extend_the_input_cell() {
        let query = CompletionQuery {
            generation: 7,
            line: "👋".to_owned(),
            cursor: "👋".len(),
            cwd: PathBuf::from("/tmp"),
        };
        let candidate = Suggestion::new(
            TextEdit {
                range: 0.."👋".len(),
                replacement: "👋🏽 wave".to_owned(),
            },
            "👋🏽 wave",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "emoji modifier",
        );
        let selection = selection(vec![candidate], 0);
        let mut screen = screen(30, 6, "> 👋".as_bytes());
        let before = screen.row_text(0).unwrap();
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.ghost().is_none());
        let _ = screen.observe(frame.transaction().bytes());
        assert_eq!(screen.row_text(0).as_deref(), Some(before.as_str()));
    }

    #[test]
    fn bidi_controls_are_replaced_before_rendering() {
        let screen = screen(76, 10, b"> git");
        let query = query("git");
        let candidate = Suggestion::new(
            TextEdit {
                range: 0..3,
                replacement: "git status".to_owned(),
            },
            "git\u{200f} status",
            "Greendale\u{61c} coursework",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            "bidi",
        );
        let selection = selection(vec![candidate], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let output = std::str::from_utf8(frame.transaction().bytes()).unwrap();
        assert!(!output.contains('\u{200f}'));
        assert!(!output.contains('\u{61c}'));
        assert!(output.contains('�'));
    }

    #[test]
    fn delayed_wrap_suppresses_overlay_and_remains_authoritative() {
        let mut screen = screen(8, 3, b"12345678");
        let cursor = screen.snapshot().cursor();
        screen.synchronize(cursor).unwrap();
        let query = query("git");
        let selection = selection(vec![suggestion("git status", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.transaction().is_empty());
        assert!(screen.snapshot().wrap_pending());
        let _ = screen.observe(b"X");
        assert_eq!(screen.row_text(0).as_deref(), Some("12345678"));
        assert_eq!(screen.row_text(1).as_deref(), Some("X"));
    }

    #[test]
    fn ghost_requires_end_cursor_safe_exact_prefix_and_never_wraps() {
        let screen = screen(12, 6, b"> git");
        let mut query = query("git");
        let selection = selection(vec![suggestion("git checkout", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        let ghost = frame.ghost().unwrap();
        assert!(ghost.clipped());
        assert_eq!(ghost.accepted_suffix(), " chec");
        assert!(ghost.cells() <= 6);

        query.cursor = 1;
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.ghost().is_none());
    }

    #[test]
    fn multi_line_suffix_renders_no_ghost() {
        let screen = screen(40, 6, b"> git");
        let query = query("git");
        let selection = selection(vec![suggestion("git one\ntwo", "", "git")], 0);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Classic),
            )
            .unwrap();
        assert!(frame.ghost().is_none());
        assert!(!frame.transaction().bytes().contains(&b'\n'));
    }

    #[test]
    fn stable_window_shows_position_total_and_plain_fallback() {
        let screen = screen(30, 6, b"> git");
        let query = query("git");
        let candidates = (0..8)
            .map(|index| suggestion(&format!("git item-{index}"), "description", "unknown"))
            .collect();
        let selection = selection(candidates, 6);
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                ui(Style::Modern),
            )
            .unwrap();
        let text = std::str::from_utf8(frame.transaction().bytes()).unwrap();
        assert!(text.contains("7/8"));
        assert!(text.contains("? [spec]"));
        assert!(!text.contains('󰆍'));
    }

    #[test]
    fn direct_options_are_clamped_to_owned_and_output_limits() {
        let mut screen = screen(80, 80, b"\x1b[10;1H> git");
        let query = query("git");
        let candidates = (0..80)
            .map(|index| suggestion(&format!("git course-{index}"), "Greendale", "git"))
            .collect();
        let selection = selection(candidates, 0);
        let options = OverlayOptions {
            style: Style::Modern,
            nerd_fonts: false,
            ghost_text: true,
            max_height: u16::MAX,
            min_usable_height: 0,
        };
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(
                screen.snapshot(),
                OverlayRequest::new(&query, &selection),
                options,
            )
            .unwrap();
        assert!(frame.transaction().bytes().len() <= MAX_RENDER_BYTES);
        apply_frame(&mut renderer, &mut screen, &frame);
        assert!(
            renderer
                .owned_region()
                .is_some_and(|owned| owned.spans().len() <= MAX_OWNED_SPANS)
        );
    }

    #[test]
    fn output_failure_hide_and_debug_are_bounded_and_private() {
        let mut screen = screen(80, 20, b"> git");
        let query = query("git");
        let selection = selection(vec![suggestion("git hunter2", "secret", "git")], 0);
        let request = OverlayRequest::new(&query, &selection).with_footer_hint("secret-hint");
        let mut renderer = OverlayRenderer::new();
        let frame = renderer
            .render(screen.snapshot(), request, ui(Style::Modern))
            .unwrap();
        assert!(frame.transaction().bytes().len() <= MAX_RENDER_BYTES);
        let debug = format!("{renderer:?} {frame:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("secret"));
        apply_frame(&mut renderer, &mut screen, &frame);

        let clear = renderer.before_shell_output(screen.snapshot()).unwrap();
        assert!(clear.transaction().bytes().len() <= MAX_RENDER_BYTES);
        let _ = screen.observe(&clear.transaction().bytes()[..1]);
        let recovery = renderer.on_failure(screen.snapshot()).unwrap();
        assert!(recovery.transaction().bytes().len() <= MAX_RENDER_BYTES);
    }

    #[test]
    fn display_sanitizing_replaces_every_invisible_character() {
        let hidden = [
            '\u{00AD}',
            '\u{200B}',
            '\u{200D}',
            '\u{200E}',
            '\u{202E}',
            '\u{2060}',
            '\u{2066}',
            '\u{FEFF}',
            '\u{E0041}',
            '\u{1D173}',
        ];
        for character in hidden {
            let (sanitized, truncated) = sanitize_bounded(&format!("git{character}status"));
            assert!(!truncated);
            assert_eq!(
                sanitized, "git\u{FFFD}status",
                "kept U+{:04X} in displayed text",
                character as u32
            );
        }

        assert_eq!(sanitize_bounded("git commit café").0, "git commit café");
    }
}
