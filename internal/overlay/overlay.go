// Package overlay provides cell-aware, non-destructive terminal overlay rendering.
//
// Rendering produces inert, bounded byte transactions. The caller owns the
// single serialized write boundary shared with child output and must clear an
// existing transaction before forwarding child bytes.
package overlay

import (
	"bytes"
	"fmt"
	"math"
	"strings"
	"sync/atomic"
	"unicode"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
	"github.com/rivo/uniseg"
	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/screen"
	"github.com/rselbach/argmax/internal/selection"
)

const (
	// MaxMenuWidth is the default and maximum menu width in terminal cells.
	MaxMenuWidth uint16 = 76
	// MinUsableMenuHeight is the minimum useful menu height.
	MinUsableMenuHeight uint16 = 3
	// MaxMenuHeight is the maximum configured candidate rows accepted by the renderer.
	MaxMenuHeight uint16 = 50
	// MaxRenderBytes is the maximum size of one serialized render transaction.
	MaxRenderBytes = 32 * 1024
	// MaxDisplaySourceBytes is the maximum source bytes inspected for a displayed field.
	MaxDisplaySourceBytes = 4 * 1024
	// MaxOwnedSpans is the maximum independently owned menu, footer, and ghost spans.
	MaxOwnedSpans = int(MaxMenuHeight) + 2
)

var nextRendererID atomic.Uint64

func init() {
	nextRendererID.Store(1)
}

// Style identifies a shipped overlay presentation.
type Style uint8

const (
	// StyleModern uses richer icons and decoration.
	StyleModern Style = iota + 1
	// StyleClassic uses a restrained compatibility-oriented presentation.
	StyleClassic
)

// Options contains bounded renderer settings resolved from UI configuration.
type Options struct {
	// Style selects modern or classic presentation.
	Style Style
	// NerdFonts permits known Nerd Font marker glyphs.
	NerdFonts bool
	// GhostText permits a safe selected suffix beside the shell cursor.
	GhostText bool
	// MaxHeight limits visible candidate rows.
	MaxHeight uint16
	// MinUsableHeight is the minimum row count at which a menu is useful.
	MinUsableHeight uint16
}

// DefaultOptions returns the default modern overlay settings.
func DefaultOptions() Options {
	return Options{
		Style:           StyleModern,
		NerdFonts:       true,
		GhostText:       true,
		MaxHeight:       15,
		MinUsableHeight: MinUsableMenuHeight,
	}
}

func (options Options) bounded() Options {
	if options.Style != StyleModern && options.Style != StyleClassic {
		options.Style = StyleModern
	}
	options.MaxHeight = clamp(options.MaxHeight, MinUsableMenuHeight, MaxMenuHeight)
	options.MinUsableHeight = clamp(options.MinUsableHeight, MinUsableMenuHeight, MaxMenuHeight)
	return options
}

func clamp(value, minimum, maximum uint16) uint16 {
	return min(max(value, minimum), maximum)
}

// Request is authority-checked state for one redraw.
type Request struct {
	query      completion.CompletionQuery
	selection  *selection.SelectionState
	footerHint string
}

// NewRequest creates a request using a query and selection from the same authority.
func NewRequest(query completion.CompletionQuery, state *selection.SelectionState) Request {
	return Request{query: query, selection: state, footerHint: "Tab insert  Esc hide"}
}

// WithFooterHint returns a request with a caller-resolved shortcut hint.
// The hint is bounded and sanitized only when rendered.
func (request Request) WithFooterHint(hint string) Request {
	request.footerHint = hint
	return request
}

// String returns a content-redacted request representation.
func (request Request) String() string {
	candidateCount := 0
	selected := "None"
	generation := uint64(0)
	if request.selection != nil {
		candidateCount = request.selection.CandidateCount()
		generation = request.selection.Generation()
		if index, ok := request.selection.SelectedIndex(); ok {
			selected = fmt.Sprintf("Some(%d)", index)
		}
	}
	return fmt.Sprintf(
		"overlay.Request{query_generation:%d, selection_generation:%d, query_bytes:%d, candidate_count:%d, selected:%s, footer_hint_bytes:%d}",
		request.query.Generation(), generation, len(request.query.Line()), candidateCount,
		selected, len(request.footerHint),
	)
}

// GoString returns a content-redacted request representation.
func (request Request) GoString() string { return request.String() }

// OwnedSpanKind identifies the purpose of owned terminal cells.
type OwnedSpanKind uint8

const (
	// SpanMenu identifies a candidate menu row.
	SpanMenu OwnedSpanKind = iota + 1
	// SpanFooter identifies a position and shortcut footer.
	SpanFooter
	// SpanGhost identifies an inline suffix after the shell cursor.
	SpanGhost
)

// OwnedSpan is an exact contiguous range of terminal cells owned by argmax.
type OwnedSpan struct {
	// Row is the zero-based terminal row.
	Row uint16
	// Column is the zero-based starting display-cell column.
	Column uint16
	// Cells is the number of contiguous display cells.
	Cells uint16
	// Kind identifies the semantic purpose of the span.
	Kind OwnedSpanKind
}

// OwnedRegion is the explicit region that must be cleared before shell output or redraw.
type OwnedRegion struct {
	terminalSize      screen.TerminalSize
	spans             []OwnedSpan
	ghostClearance    uint16
	hasGhostClearance bool
	restoreCursor     screen.CursorPosition
}

// TerminalSize returns the size for which the region coordinates were produced.
func (region OwnedRegion) TerminalSize() screen.TerminalSize { return region.terminalSize }

// Spans returns a copy of the bounded owned spans.
func (region OwnedRegion) Spans() []OwnedSpan {
	return append([]OwnedSpan(nil), region.spans...)
}

// String returns structural ownership state without terminal content.
func (region OwnedRegion) String() string {
	return fmt.Sprintf(
		"overlay.OwnedRegion{columns:%d, rows:%d, span_count:%d, restore_row:%d, restore_column:%d}",
		region.terminalSize.Columns(), region.terminalSize.Rows(), len(region.spans),
		region.restoreCursor.Row(), region.restoreCursor.Column(),
	)
}

// GoString returns structural ownership state without terminal content.
func (region OwnedRegion) GoString() string { return region.String() }

func (region OwnedRegion) clone() OwnedRegion {
	region.spans = append([]OwnedSpan(nil), region.spans...)
	return region
}

// DisplayedGhost describes the safe visible portion of a selected suffix.
type DisplayedGhost struct {
	acceptedSuffix string
	cells          uint16
	clipped        bool
}

// AcceptedSuffix returns the exact suffix represented before any visual ellipsis.
func (ghost DisplayedGhost) AcceptedSuffix() string { return ghost.acceptedSuffix }

// Cells returns occupied display cells, including a clipping ellipsis.
func (ghost DisplayedGhost) Cells() uint16 { return ghost.cells }

// Clipped reports whether the full selected suffix did not fit.
func (ghost DisplayedGhost) Clipped() bool { return ghost.clipped }

// String returns content-redacted ghost metadata.
func (ghost DisplayedGhost) String() string {
	return fmt.Sprintf(
		"overlay.DisplayedGhost{accepted_bytes:%d, cells:%d, clipped:%t}",
		len(ghost.acceptedSuffix), ghost.cells, ghost.clipped,
	)
}

// GoString returns content-redacted ghost metadata.
func (ghost DisplayedGhost) GoString() string { return ghost.String() }

// TransactionKind identifies the purpose of a serialized renderer transaction.
type TransactionKind uint8

const (
	// TransactionDraw clears prior cells and draws a new overlay.
	TransactionDraw TransactionKind = iota + 1
	// TransactionClear clears prior cells without drawing a replacement.
	TransactionClear
	// TransactionSuppressed emits no bytes because drawing was unsafe.
	TransactionSuppressed
)

// RenderTransaction is one indivisible terminal-write transaction.
// After fully writing non-empty Bytes, pass the transaction to Renderer.Acknowledge.
type RenderTransaction struct {
	rendererID uint64
	sequence   uint64
	kind       TransactionKind
	bytes      []byte
}

// Sequence returns the renderer-local serialization token.
func (transaction RenderTransaction) Sequence() uint64 { return transaction.sequence }

// Kind returns the draw, clear, or suppressed disposition.
func (transaction RenderTransaction) Kind() TransactionKind { return transaction.kind }

// Bytes returns a copy of the inert ANSI bytes.
func (transaction RenderTransaction) Bytes() []byte { return bytes.Clone(transaction.bytes) }

// Empty reports whether the transaction writes no terminal bytes.
func (transaction RenderTransaction) Empty() bool { return len(transaction.bytes) == 0 }

// String returns content-redacted transaction metadata.
func (transaction RenderTransaction) String() string {
	return fmt.Sprintf(
		"overlay.RenderTransaction{sequence:%d, kind:%d, byte_count:%d}",
		transaction.sequence, transaction.kind, len(transaction.bytes),
	)
}

// GoString returns content-redacted transaction metadata.
func (transaction RenderTransaction) GoString() string { return transaction.String() }

// Frame contains serialized terminal output and safe ghost acceptance metadata.
type Frame struct {
	transaction RenderTransaction
	ghost       DisplayedGhost
	hasGhost    bool
}

// Transaction returns the serialized terminal output.
func (frame Frame) Transaction() RenderTransaction { return frame.transaction }

// Ghost returns exact visible ghost suffix metadata when present.
func (frame Frame) Ghost() (DisplayedGhost, bool) { return frame.ghost, frame.hasGhost }

// String returns content-redacted frame metadata.
func (frame Frame) String() string {
	ghost := "None"
	if frame.hasGhost {
		ghost = frame.ghost.String()
	}
	return fmt.Sprintf("overlay.Frame{transaction:%s, ghost:%s}", frame.transaction, ghost)
}

// GoString returns content-redacted frame metadata.
func (frame Frame) GoString() string { return frame.String() }

// ErrorKind identifies a bounded rendering failure.
type ErrorKind uint8

const (
	// RendererIDExhausted indicates process-wide renderer identities are exhausted.
	RendererIDExhausted ErrorKind = iota + 1
	// SequenceExhausted indicates renderer-local transaction identities are exhausted.
	SequenceExhausted
	// OutputLimit indicates a transaction exceeded MaxRenderBytes and was not emitted.
	OutputLimit
	// TransactionPending indicates a write must be acknowledged or recovered first.
	TransactionPending
	// UnexpectedTransaction indicates an acknowledgment was not for the latest transaction.
	UnexpectedTransaction
	// ForeignTransaction indicates a transaction belongs to another renderer.
	ForeignTransaction
)

// Error is a bounded, content-redacted renderer failure.
type Error struct {
	kind     ErrorKind
	sequence uint64
	expected uint64
	actual   uint64
}

// Kind returns the rendering failure class.
func (err *Error) Kind() ErrorKind { return err.kind }

// Sequence returns the pending sequence for TransactionPending.
func (err *Error) Sequence() uint64 { return err.sequence }

// Expected returns the latest sequence for UnexpectedTransaction.
func (err *Error) Expected() uint64 { return err.expected }

// Actual returns the supplied sequence for UnexpectedTransaction.
func (err *Error) Actual() uint64 { return err.actual }

// Error describes the failure without terminal or candidate content.
func (err *Error) Error() string {
	switch err.kind {
	case RendererIDExhausted:
		return "overlay renderer IDs are exhausted"
	case SequenceExhausted:
		return "overlay sequence is exhausted"
	case OutputLimit:
		return "overlay output exceeded its byte limit"
	case TransactionPending:
		return fmt.Sprintf("overlay transaction %d is still pending", err.sequence)
	case UnexpectedTransaction:
		return fmt.Sprintf(
			"overlay transaction %d cannot acknowledge latest transaction %d",
			err.actual, err.expected,
		)
	case ForeignTransaction:
		return "overlay transaction belongs to a different renderer"
	default:
		return "overlay rendering failed"
	}
}

// GoString returns a content-redacted error representation.
func (err *Error) GoString() string { return err.Error() }

type ansiBuilder struct {
	bytes []byte
}

func (builder *ansiBuilder) push(value string) error {
	if len(value) > MaxRenderBytes-len(builder.bytes) {
		return &Error{kind: OutputLimit}
	}
	builder.bytes = append(builder.bytes, value...)
	return nil
}

func (builder *ansiBuilder) eraseCells(cells uint16) error {
	return builder.push(fmt.Sprintf("\x1b[%dX", cells))
}

func (builder *ansiBuilder) cup(row, column uint16) error {
	return builder.push(fmt.Sprintf("\x1b[%d;%dH", uint32(row)+1, uint32(column)+1))
}

func (builder *ansiBuilder) finish() []byte { return builder.bytes }

type menuPlan struct {
	startRow    uint16
	startColumn uint16
	width       uint16
	itemRows    uint16
	footer      bool
}

type plannedGhost struct {
	visual   string
	accepted string
	cells    uint16
	clipped  bool
}

type pendingTransaction struct {
	sequence  uint64
	recovery  OwnedRegion
	nextOwned *OwnedRegion
}

type noCopy struct{}

func (*noCopy) Lock()   {}
func (*noCopy) Unlock() {}

// Renderer owns prior terminal cells and a stable candidate window.
// A Renderer must not be copied after first use or used concurrently.
type Renderer struct {
	noCopy              noCopy
	owned               *OwnedRegion
	rendererID          uint64
	sequence            uint64
	windowGeneration    uint64
	hasWindowGeneration bool
	windowStart         int
	pending             *pendingTransaction
}

// NewRenderer creates an empty renderer.
func NewRenderer() *Renderer { return &Renderer{} }

// OwnedRegion returns a copy of the region owned after the latest acknowledged transaction.
func (renderer *Renderer) OwnedRegion() (OwnedRegion, bool) {
	if renderer == nil || renderer.owned == nil {
		return OwnedRegion{}, false
	}
	return renderer.owned.clone(), true
}

// String returns content-redacted renderer state.
func (renderer *Renderer) String() string {
	if renderer == nil {
		return "overlay.Renderer(<nil>)"
	}
	spanCount := 0
	if renderer.owned != nil {
		spanCount = len(renderer.owned.spans)
	}
	pending := "None"
	if renderer.pending != nil {
		pending = fmt.Sprintf("Some(%d)", renderer.pending.sequence)
	}
	generation := "None"
	if renderer.hasWindowGeneration {
		generation = fmt.Sprintf("Some(%d)", renderer.windowGeneration)
	}
	return fmt.Sprintf(
		"overlay.Renderer{owned_span_count:%d, sequence:%d, window_generation:%s, window_start:%d, pending_sequence:%s}",
		spanCount, renderer.sequence, generation, renderer.windowStart, pending,
	)
}

// GoString returns content-redacted renderer state.
func (renderer *Renderer) GoString() string { return renderer.String() }

// Acknowledge records that every byte of the latest transaction was written.
// Empty latest transactions may also be acknowledged for uniform caller logic.
func (renderer *Renderer) Acknowledge(transaction RenderTransaction) error {
	if renderer.rendererID == 0 || renderer.rendererID != transaction.rendererID {
		return &Error{kind: ForeignTransaction}
	}
	expected := renderer.sequence
	if renderer.pending != nil {
		expected = renderer.pending.sequence
	}
	if transaction.sequence != expected {
		return &Error{
			kind: UnexpectedTransaction, expected: expected, actual: transaction.sequence,
		}
	}
	if renderer.pending != nil {
		pending := renderer.pending
		renderer.pending = nil
		if pending.nextOwned == nil {
			renderer.owned = nil
		} else {
			next := pending.nextOwned.clone()
			renderer.owned = &next
		}
	}
	return nil
}

// Render clears prior cells and draws a menu and/or ghost when safe.
func (renderer *Renderer) Render(
	snapshot screen.ScreenSnapshot,
	request Request,
	options Options,
) (Frame, error) {
	if err := renderer.ensureTransactionComplete(); err != nil {
		return Frame{}, err
	}
	options = options.bounded()
	if request.selection == nil || !snapshot.OverlaySafe() ||
		!request.selection.IsVisible() ||
		request.query.Generation() != request.selection.Generation() {
		return renderer.suppressOrClear(snapshot)
	}

	candidates := request.selection.Candidates()
	selected, hasSelected := request.selection.SelectedIndex()
	menuAreaClear := snapshot.RowsBelowClear() || renderer.canReuseMenuArea(snapshot)
	var menu *menuPlan
	if hasSelected {
		menu = planMenu(snapshot, len(candidates), options, menuAreaClear)
	}
	ghostClearance := renderer.ghostClearance(snapshot)
	var ghost *plannedGhost
	if options.GhostText && hasSelected && selected >= 0 && selected < len(candidates) {
		ghost = planGhost(request.query, candidates[selected], ghostClearance)
	}
	if menu == nil && ghost == nil {
		return renderer.Clear(snapshot)
	}

	rendererID, sequence, err := renderer.nextTransaction()
	if err != nil {
		return Frame{}, err
	}
	builder := &ansiBuilder{}
	recoverySpans := make([]OwnedSpan, 0, MaxOwnedSpans*2)
	if renderer.owned != nil {
		recoverySpans = append(recoverySpans, renderer.owned.spans...)
	}
	if err := renderer.clearOwnedSpans(builder, snapshot.Size()); err != nil {
		return Frame{}, err
	}

	spans := make([]OwnedSpan, 0, MaxOwnedSpans)
	if menu != nil && hasSelected {
		if err := renderer.drawMenu(
			builder, &spans, *menu, request, candidates, options, selected,
		); err != nil {
			return Frame{}, err
		}
	}
	if ghost != nil {
		cursor := snapshot.Cursor()
		if err := builder.cup(cursor.Row(), cursor.Column()); err != nil {
			return Frame{}, err
		}
		if err := builder.push(ghost.visual); err != nil {
			return Frame{}, err
		}
		spans = append(spans, OwnedSpan{
			Row: cursor.Row(), Column: cursor.Column(), Cells: ghost.cells, Kind: SpanGhost,
		})
	}
	if err := builder.cup(snapshot.Cursor().Row(), snapshot.Cursor().Column()); err != nil {
		return Frame{}, err
	}

	recoverySpans = append(recoverySpans, spans...)
	recovery := OwnedRegion{
		terminalSize: snapshot.Size(), spans: recoverySpans,
		restoreCursor: snapshot.Cursor(),
	}
	nextOwned := OwnedRegion{
		terminalSize: snapshot.Size(), spans: spans, restoreCursor: snapshot.Cursor(),
	}
	if ghost != nil {
		nextOwned.ghostClearance = ghostClearance
		nextOwned.hasGhostClearance = true
	}
	renderer.pending = &pendingTransaction{
		sequence: sequence, recovery: recovery, nextOwned: &nextOwned,
	}
	frame := Frame{transaction: RenderTransaction{
		rendererID: rendererID, sequence: sequence, kind: TransactionDraw,
		bytes: builder.finish(),
	}}
	if ghost != nil {
		frame.ghost = DisplayedGhost{
			acceptedSuffix: ghost.accepted, cells: ghost.cells, clipped: ghost.clipped,
		}
		frame.hasGhost = true
	}
	return frame, nil
}

// Clear clears every prior owned cell before hiding the overlay.
func (renderer *Renderer) Clear(snapshot screen.ScreenSnapshot) (Frame, error) {
	if err := renderer.ensureTransactionComplete(); err != nil {
		return Frame{}, err
	}
	rendererID, sequence, err := renderer.nextTransaction()
	if err != nil {
		return Frame{}, err
	}
	if renderer.owned == nil {
		return emptyFrame(rendererID, sequence, TransactionClear), nil
	}
	if !canAddress(snapshot) {
		return emptyFrame(rendererID, sequence, TransactionSuppressed), nil
	}

	recovery := renderer.owned.clone()
	builder := &ansiBuilder{}
	if err := renderer.clearOwnedSpans(builder, snapshot.Size()); err != nil {
		return Frame{}, err
	}
	if err := builder.cup(snapshot.Cursor().Row(), snapshot.Cursor().Column()); err != nil {
		return Frame{}, err
	}
	recovery.terminalSize = snapshot.Size()
	recovery.restoreCursor = snapshot.Cursor()
	renderer.pending = &pendingTransaction{sequence: sequence, recovery: recovery}
	return Frame{transaction: RenderTransaction{
		rendererID: rendererID, sequence: sequence, kind: TransactionClear,
		bytes: builder.finish(),
	}}, nil
}

// BeforeShellOutput clears owned cells before serialized child output is forwarded.
func (renderer *Renderer) BeforeShellOutput(snapshot screen.ScreenSnapshot) (Frame, error) {
	return renderer.Clear(snapshot)
}

// OnResize clears the intersecting old region after a terminal resize.
func (renderer *Renderer) OnResize(snapshot screen.ScreenSnapshot) (Frame, error) {
	return renderer.Clear(snapshot)
}

// OnFailure clears prior cells after a renderer or session write failure.
// It may replace an unacknowledged transaction after a partial write.
func (renderer *Renderer) OnFailure(snapshot screen.ScreenSnapshot) (Frame, error) {
	rendererID, sequence, err := renderer.nextTransaction()
	if err != nil {
		return Frame{}, err
	}
	var recovery *OwnedRegion
	if renderer.pending != nil && renderer.pending.recovery.terminalSize == snapshot.Size() {
		value := renderer.pending.recovery.clone()
		recovery = &value
	} else if renderer.owned != nil && renderer.owned.terminalSize == snapshot.Size() {
		value := renderer.owned.clone()
		recovery = &value
	}
	if recovery == nil {
		renderer.pending = nil
		renderer.owned = nil
		return emptyFrame(rendererID, sequence, TransactionClear), nil
	}
	if !canRecover(snapshot) {
		renderer.pending = nil
		renderer.owned = nil
		return emptyFrame(rendererID, sequence, TransactionSuppressed), nil
	}

	builder := &ansiBuilder{}
	if err := clearSpans(builder, recovery.spans, snapshot.Size()); err != nil {
		return Frame{}, err
	}
	if err := builder.cup(recovery.restoreCursor.Row(), recovery.restoreCursor.Column()); err != nil {
		return Frame{}, err
	}
	renderer.pending = &pendingTransaction{sequence: sequence, recovery: recovery.clone()}
	return Frame{transaction: RenderTransaction{
		rendererID: rendererID, sequence: sequence, kind: TransactionClear,
		bytes: builder.finish(),
	}}, nil
}

func (renderer *Renderer) suppressOrClear(snapshot screen.ScreenSnapshot) (Frame, error) {
	if canAddress(snapshot) {
		return renderer.Clear(snapshot)
	}
	rendererID, sequence, err := renderer.nextTransaction()
	if err != nil {
		return Frame{}, err
	}
	return emptyFrame(rendererID, sequence, TransactionSuppressed), nil
}

func (renderer *Renderer) nextTransaction() (uint64, uint64, error) {
	if renderer.sequence == math.MaxUint64 {
		return 0, 0, &Error{kind: SequenceExhausted}
	}
	sequence := renderer.sequence + 1
	rendererID := renderer.rendererID
	if rendererID == 0 {
		var err error
		rendererID, err = allocateRendererID(&nextRendererID)
		if err != nil {
			return 0, 0, err
		}
		renderer.rendererID = rendererID
	}
	renderer.sequence = sequence
	return rendererID, sequence, nil
}

func (renderer *Renderer) ensureTransactionComplete() error {
	if renderer.pending == nil {
		return nil
	}
	return &Error{kind: TransactionPending, sequence: renderer.pending.sequence}
}

func (renderer *Renderer) clearOwnedSpans(builder *ansiBuilder, size screen.TerminalSize) error {
	if renderer.owned == nil {
		return nil
	}
	return clearSpans(builder, renderer.owned.spans, size)
}

func (renderer *Renderer) canReuseMenuArea(snapshot screen.ScreenSnapshot) bool {
	if renderer.owned == nil || renderer.owned.terminalSize != snapshot.Size() {
		return false
	}
	wantRow := saturatingAdd(snapshot.Cursor().Row(), 1)
	for _, span := range renderer.owned.spans {
		if span.Kind == SpanMenu && span.Row == wantRow {
			return true
		}
	}
	return false
}

func (renderer *Renderer) ghostClearance(snapshot screen.ScreenSnapshot) uint16 {
	tracked := snapshot.BlankCellsToRight()
	if renderer.owned != nil && renderer.owned.terminalSize == snapshot.Size() {
		for _, span := range renderer.owned.spans {
			if span.Kind == SpanGhost && span.Row == snapshot.Cursor().Row() &&
				span.Column == snapshot.Cursor().Column() && renderer.owned.hasGhostClearance {
				tracked = renderer.owned.ghostClearance
				break
			}
		}
	}
	beforeLastColumn := snapshot.Size().Columns() - snapshot.Cursor().Column() - 1
	return min(tracked, beforeLastColumn)
}

func (renderer *Renderer) drawMenu(
	builder *ansiBuilder,
	spans *[]OwnedSpan,
	plan menuPlan,
	request Request,
	candidates []completion.Suggestion,
	options Options,
	selected int,
) error {
	capacity := int(plan.itemRows)
	start := renderer.stableWindow(
		request.selection.Generation(), len(candidates), selected, capacity,
	)
	end := min(start+capacity, len(candidates))
	for index := start; index < end; index++ {
		row := plan.startRow + uint16(index-start)
		text := menuRow(candidates[index], index == selected, plan.width, options)
		if err := builder.cup(row, plan.startColumn); err != nil {
			return err
		}
		if err := builder.push(text); err != nil {
			return err
		}
		*spans = append(*spans, OwnedSpan{
			Row: row, Column: plan.startColumn, Cells: plan.width, Kind: SpanMenu,
		})
	}
	if plan.footer {
		row := plan.startRow + plan.itemRows
		footer := footerRow(
			selected, len(candidates), capacity, request.footerHint, plan.width, options.Style,
		)
		if err := builder.cup(row, plan.startColumn); err != nil {
			return err
		}
		if err := builder.push(footer); err != nil {
			return err
		}
		*spans = append(*spans, OwnedSpan{
			Row: row, Column: plan.startColumn, Cells: plan.width, Kind: SpanFooter,
		})
	}
	return nil
}

func (renderer *Renderer) stableWindow(generation uint64, total, selected, capacity int) int {
	if !renderer.hasWindowGeneration || renderer.windowGeneration != generation {
		renderer.hasWindowGeneration = true
		renderer.windowGeneration = generation
		renderer.windowStart = 0
	}
	capacity = max(capacity, 1)
	switch {
	case selected < renderer.windowStart:
		renderer.windowStart = selected
	case selected >= saturatingIntAdd(renderer.windowStart, capacity):
		renderer.windowStart = selected + 1 - capacity
	}
	renderer.windowStart = min(renderer.windowStart, max(total-capacity, 0))
	return renderer.windowStart
}

func clearSpans(builder *ansiBuilder, spans []OwnedSpan, size screen.TerminalSize) error {
	for _, span := range spans {
		if span.Row >= size.Rows() || span.Column >= size.Columns() {
			continue
		}
		cells := min(span.Cells, size.Columns()-span.Column)
		if cells == 0 {
			continue
		}
		if err := builder.cup(span.Row, span.Column); err != nil {
			return err
		}
		if err := builder.eraseCells(cells); err != nil {
			return err
		}
	}
	return nil
}

func allocateRendererID(counter *atomic.Uint64) (uint64, error) {
	for {
		current := counter.Load()
		if current == math.MaxUint64 {
			return 0, &Error{kind: RendererIDExhausted}
		}
		if current == 0 {
			return 0, &Error{kind: RendererIDExhausted}
		}
		if counter.CompareAndSwap(current, current+1) {
			return current, nil
		}
	}
}

func emptyFrame(rendererID, sequence uint64, kind TransactionKind) Frame {
	return Frame{transaction: RenderTransaction{
		rendererID: rendererID, sequence: sequence, kind: kind,
	}}
}

func canAddress(snapshot screen.ScreenSnapshot) bool {
	return snapshot.Synchronized() && canRecover(snapshot)
}

func canRecover(snapshot screen.ScreenSnapshot) bool {
	return snapshot.Buffer() == screen.Primary &&
		snapshot.ScrollRegion().IsFull(snapshot.Size()) &&
		!snapshot.OriginMode() && !snapshot.InsertMode() && !snapshot.WrapPending()
}

func planMenu(
	snapshot screen.ScreenSnapshot,
	total int,
	options Options,
	menuAreaClear bool,
) *menuPlan {
	if total == 0 || !menuAreaClear || !snapshot.ScrollRegion().IsFull(snapshot.Size()) {
		return nil
	}
	size := snapshot.Size()
	if size.Rows() <= options.MinUsableHeight || size.Columns() < 8 {
		return nil
	}
	desiredItems := min(total, int(options.MaxHeight))
	footer := options.Style == StyleModern || total > desiredItems
	desiredRows := uint16(desiredItems)
	if footer {
		desiredRows++
	}
	below := size.Rows() - snapshot.Cursor().Row() - 1
	available := min(below, desiredRows)
	minimum := min(options.MinUsableHeight, desiredRows)
	if available < minimum {
		return nil
	}
	footer = footer && available >= 2
	itemRows := available
	if footer {
		itemRows--
	}
	itemRows = min(itemRows, uint16(desiredItems))
	if itemRows == 0 {
		return nil
	}
	width := min(MaxMenuWidth, size.Columns()-1)
	if width == 0 {
		return nil
	}
	startColumn := min(snapshot.Cursor().Column(), size.Columns()-width-1)
	return &menuPlan{
		startRow: snapshot.Cursor().Row() + 1, startColumn: startColumn,
		width: width, itemRows: itemRows, footer: footer,
	}
}

func planGhost(
	query completion.CompletionQuery,
	candidate completion.Suggestion,
	available uint16,
) *plannedGhost {
	if query.Cursor() != len(query.Line()) || len(query.Line()) > MaxDisplaySourceBytes ||
		len(candidate.Edit().Replacement()) > MaxDisplaySourceBytes {
		return nil
	}
	result, err := candidate.ResultingLine(query)
	if err != nil {
		return nil
	}
	suffix, ok := selection.GhostSuffix(query.Line(), result)
	if !ok || len(suffix) > MaxDisplaySourceBytes || !graphemeBoundary(result, len(query.Line())) {
		return nil
	}
	first, _ := utf8.DecodeRuneInString(suffix)
	if first == utf8.RuneError && len(suffix) == 0 {
		return nil
	}
	for _, character := range suffix {
		if unsafeDisplayCharacter(character) {
			return nil
		}
	}
	if textWidth(string(first)) == 0 ||
		(first >= '\U0001f1e6' && first <= '\U0001f1ff') ||
		(first >= '\U0001f3fb' && first <= '\U0001f3ff') || available < 2 {
		return nil
	}
	return clipExactGhost(suffix, available)
}

func graphemeBoundary(value string, target int) bool {
	graphemes := uniseg.NewGraphemes(value)
	for graphemes.Next() {
		start, _ := graphemes.Positions()
		if start == target {
			return true
		}
	}
	return false
}

func clipExactGhost(value string, available uint16) *plannedGhost {
	width := textWidth(value)
	if width == 0 {
		return nil
	}
	if width <= int(available) {
		return &plannedGhost{
			visual: value, accepted: value, cells: uint16(width), clipped: false,
		}
	}

	limit := int(available - 1)
	var accepted strings.Builder
	cells := 0
	graphemes := uniseg.NewGraphemes(value)
	for graphemes.Next() {
		grapheme := graphemes.Str()
		width := textWidth(grapheme)
		if cells+width > limit {
			break
		}
		accepted.WriteString(grapheme)
		cells += width
	}
	if accepted.Len() == 0 {
		return nil
	}
	acceptedText := accepted.String()
	return &plannedGhost{
		visual: acceptedText + "…", accepted: acceptedText,
		cells: uint16(cells + 1), clipped: true,
	}
}

func menuRow(
	candidate completion.Suggestion,
	selected bool,
	width uint16,
	options Options,
) string {
	marker := " "
	if selected {
		marker = ">"
	}
	icon := iconMarker(candidate.Icon(), candidate.Source(), options.NerdFonts)
	var prefix string
	if options.Style == StyleModern {
		prefix = fmt.Sprintf("%s %s [%s] ", marker, icon, candidate.Source().Badge())
	} else {
		prefix = fmt.Sprintf("%s [%s] ", marker, candidate.Source().Badge())
	}
	available := max(int(width)-textWidth(prefix), 0)
	var row strings.Builder
	row.WriteString(prefix)
	if options.Style == StyleModern && candidate.Description() != "" && available >= 12 {
		const separator = " — "
		separatorWidth := textWidth(separator)
		descriptionWidth := max(available/3, 4)
		commandWidth := max(available-separatorWidth-descriptionWidth, 0)
		row.WriteString(clipText(candidate.Display(), commandWidth))
		row.WriteString(separator)
		row.WriteString(clipText(candidate.Description(), descriptionWidth))
	} else {
		row.WriteString(clipText(candidate.Display(), available))
	}
	return padOrClip(row.String(), int(width))
}

func footerRow(selected, total, capacity int, hint string, width uint16, style Style) string {
	position := fmt.Sprintf("%d/%d", saturatingIntAdd(selected, 1), total)
	var content string
	if style == StyleModern {
		sanitized, _ := sanitizeBounded(hint)
		if total > capacity {
			content = fmt.Sprintf(" %s  ↑/↓ navigate  %s", position, sanitized)
		} else {
			content = fmt.Sprintf(" %s  %s", position, sanitized)
		}
	} else {
		content = " " + position
	}
	return padOrClip(content, int(width))
}

func iconMarker(icon string, source completion.SuggestionSource, nerdFonts bool) string {
	if len(icon) > 16 {
		icon = ""
	}
	if nerdFonts {
		switch {
		case strings.EqualFold(icon, "command"), strings.EqualFold(icon, "terminal"):
			return "󰆍"
		case strings.EqualFold(icon, "file"):
			return "󰈔"
		case strings.EqualFold(icon, "directory"), strings.EqualFold(icon, "folder"):
			return "󰉋"
		case strings.EqualFold(icon, "git"), strings.EqualFold(icon, "branch"):
			return "󰘬"
		case strings.EqualFold(icon, "history"):
			return "󰋚"
		case source == completion.SourceFile:
			return "󰈔"
		case source == completion.SourceHistory:
			return "󰋚"
		default:
			return "•"
		}
	}
	switch {
	case strings.EqualFold(icon, "file"):
		return "f"
	case strings.EqualFold(icon, "directory"), strings.EqualFold(icon, "folder"):
		return "d"
	case strings.EqualFold(icon, "git"), strings.EqualFold(icon, "branch"):
		return "g"
	case strings.EqualFold(icon, "history"):
		return "h"
	case strings.EqualFold(icon, "command"), strings.EqualFold(icon, "terminal"):
		return "c"
	case source == completion.SourceFile:
		return "f"
	case source == completion.SourceHistory:
		return "h"
	default:
		return "?"
	}
}

func padOrClip(value string, cells int) string {
	value = clipText(value, cells)
	return value + strings.Repeat(" ", max(cells-textWidth(value), 0))
}

func clipText(value string, cells int) string {
	if cells == 0 {
		return ""
	}
	sanitized, sourceTruncated := sanitizeBounded(value)
	if textWidth(sanitized) <= cells && !sourceTruncated {
		return sanitized
	}

	contentLimit := max(cells-1, 0)
	var output strings.Builder
	width := 0
	graphemes := uniseg.NewGraphemes(sanitized)
	for graphemes.Next() {
		grapheme := graphemes.Str()
		graphemeWidth := textWidth(grapheme)
		if width+graphemeWidth > contentLimit {
			break
		}
		output.WriteString(grapheme)
		width += graphemeWidth
	}
	output.WriteRune('…')
	return output.String()
}

func sanitizeBounded(value string) (string, bool) {
	end := min(len(value), MaxDisplaySourceBytes)
	for end > 0 && end < len(value) && !utf8.RuneStart(value[end]) {
		end--
	}
	truncated := end != len(value)
	var sanitized strings.Builder
	sanitized.Grow(min(end, 256))
	for _, character := range value[:end] {
		if unsafeDisplayCharacter(character) {
			sanitized.WriteRune(utf8.RuneError)
		} else {
			sanitized.WriteRune(character)
		}
	}
	return sanitized.String(), truncated
}

func unsafeDisplayCharacter(character rune) bool {
	return unicode.IsControl(character) || invisibleCharacter(character)
}

func invisibleCharacter(character rune) bool {
	switch {
	case character == '\u00ad',
		character >= '\u0600' && character <= '\u0605',
		character == '\u061c', character == '\u06dd', character == '\u070f',
		character >= '\u0890' && character <= '\u0891', character == '\u08e2',
		character == '\u180e', character >= '\u200b' && character <= '\u200f',
		character >= '\u202a' && character <= '\u202e',
		character >= '\u2060' && character <= '\u2064',
		character >= '\u2066' && character <= '\u206f', character == '\ufeff',
		character >= '\ufff9' && character <= '\ufffb', character == '\U000110bd',
		character == '\U000110cd',
		character >= '\U00013430' && character <= '\U0001343f',
		character >= '\U0001bca0' && character <= '\U0001bca3',
		character >= '\U0001d173' && character <= '\U0001d17a',
		character == '\U000e0001',
		character >= '\U000e0020' && character <= '\U000e007f':
		return true
	default:
		return false
	}
}

func textWidth(value string) int { return ansi.StringWidth(value) }

func saturatingAdd(value, add uint16) uint16 {
	if math.MaxUint16-value < add {
		return math.MaxUint16
	}
	return value + add
}

func saturatingIntAdd(value, add int) int {
	if add > 0 && value > math.MaxInt-add {
		return math.MaxInt
	}
	return value + add
}
