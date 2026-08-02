package session

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
	"github.com/rselbach/argmax/internal/engine"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/overlay"
	"github.com/rselbach/argmax/internal/updater"
)

// sugResult carries a computed candidate set back to the loop.
type sugResult struct {
	gen   uint64
	open  bool // explicit open via Up/Down
	empty bool // empty-prompt predictions
	suggs []core.Suggestion
}

// aiResult carries an AI completion back to the loop.
type aiResult struct {
	gen    uint64
	buffer string
	text   string
}

// requestSuggest computes candidates off the input path (PERF-002/006);
// stale results are dropped by generation.
func (s *sess) requestSuggest(open bool) {
	if !s.menuOn || s.commandRunning() {
		return
	}
	if s.hiddenEdit && !open {
		s.clearOverlay()
		s.menuOpen = false
		return
	}
	buf := string(s.buffer)
	if buf == "" && !open {
		s.suggs = nil
		s.menuOpen = false
		s.clearOverlay()
		return
	}
	logs.Debug("suggest", "buffer", buf, "open", open, "gen", s.bufferGen)
	gen := s.bufferGen
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 800*time.Millisecond)
		var sugs []core.Suggestion
		if buf == "" {
			sugs = s.eng.HistoryRecent(15)
		} else {
			sugs = s.eng.Suggest(ctx, buf)
		}
		cancel()
		select {
		case s.sugCh <- sugResult{gen: gen, open: open, suggs: sugs}:
		default:
		}
	}()
}

// onSuggestResult applies a computed result set.
func (s *sess) onSuggestResult(res sugResult) {
	if res.gen != s.bufferGen {
		return // stale (query moved on)
	}
	if res.empty {
		s.onEmptyResult(res.suggs)
		return
	}
	s.suggs = res.suggs
	switch {
	case res.open:
		s.menuOpen = len(res.suggs) > 0
		s.explicit = s.menuOpen
		if s.menuOpen {
			s.selected = 0
			s.navigated = true
			if s.mode == core.ModeHistory {
				s.preview(res.suggs[0].Text)
			}
		}
	case !s.menuOpen && len(res.suggs) > 0 && len(s.buffer) > 0 && !s.hiddenEdit:
		// Auto-open on eligible edits.
		s.menuOpen = true
		s.selected = 0
	}
	s.clampSelection()
	s.draw()
}

func (s *sess) clampSelection() {
	if len(s.suggs) == 0 {
		s.selected = -1
		return
	}
	if s.selected < 0 {
		s.selected = 0
	}
	if s.selected >= len(s.suggs) {
		s.selected = len(s.suggs) - 1
	}
}

// requestAI fires a debounced AI completion (AI-005/006).
func (s *sess) requestAI() {
	if !s.cfg.AI.Enabled || !s.eng.AIEnabled() {
		return
	}
	if s.cursor != len(s.buffer) || s.navigated {
		return // AI-006: cursor at EOL, no navigation away from the result set
	}
	buf := string(s.buffer)
	if len(strings.Join(strings.Fields(buf), "")) < 3 {
		return // AI-005: at least three non-space characters
	}
	gen := s.bufferGen
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		text, ok := s.eng.AISuggest(ctx, buf)
		cancel()
		if !ok {
			return
		}
		select {
		case s.aiCh <- aiResult{gen: gen, buffer: buf, text: text}:
		default:
		}
	}()
}

// onAIResult injects a valid AI candidate (AI-011): never while the user
// navigated, never over a newer buffer.
func (s *sess) onAIResult(res aiResult) {
	if res.gen != s.bufferGen || res.buffer != string(s.buffer) {
		return
	}
	if s.navigated || s.cursor != len(s.buffer) {
		return
	}
	s.suggs = engine.InjectAI(s.suggs, res.text)
	if !s.menuOpen && len(s.suggs) > 0 && s.menuOn && !s.hiddenEdit {
		s.menuOpen = true
		s.selected = 0
	}
	s.draw()
}

// requestEmpty evaluates opt-in empty-prompt predictions (EMPTY-001..004).
func (s *sess) requestEmpty() {
	if len(s.buffer) != 0 || !s.atPrompt || s.commandRunning() || !s.menuOn || s.hiddenEdit {
		return
	}
	gen := s.bufferGen
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 1500*time.Millisecond)
		sugs := s.eng.EmptyPrompt(ctx)
		cancel()
		if len(sugs) == 0 {
			return
		}
		select {
		case s.sugCh <- sugResult{gen: gen, empty: true, suggs: sugs}:
		default:
		}
	}()
}

// onEmptyResult shows an empty-prompt prediction as ghost text only.
func (s *sess) onEmptyResult(sugs []core.Suggestion) {
	if len(sugs) == 0 || len(s.buffer) != 0 || !s.atPrompt || s.navigated {
		return
	}
	s.suggs = sugs
	s.selected = 0
	s.emptyPred = true
	s.outMu.Lock()
	s.clearOverlayLocked()
	_, col := s.vt.pos()
	if s.cfg.UI.GhostText {
		overlay.RenderGhost(s.out, sugs[0].Text, s.termW-col-1)
		s.ghostOn = true
	}
	s.outMu.Unlock()
}

// draw renders the menu and ghost text (UI-001..014).
func (s *sess) draw() {
	s.outMu.Lock()
	defer s.outMu.Unlock()
	s.clearOverlayLocked()

	if !s.menuOpen || len(s.suggs) == 0 || s.commandRunning() || !s.menuOn {
		return
	}

	buf := string(s.buffer)
	_, col := s.vt.pos()

	// Pre-allocate vertical space near the bottom (UI-002).
	need := s.cfg.UI.MaxHeight
	if need > len(s.suggs)+3 {
		need = len(s.suggs) + 3
	}
	s.ensureSpaceLocked(need)
	linesBelow := s.linesBelow()

	opts := overlay.RenderOpts{
		TermWidth:  s.termW,
		TermHeight: s.termH,
		StartCol:   col,
		LinesBelow: linesBelow,
		Style:      s.cfg.UI.Style,
		NerdFonts:  s.cfg.UI.NerdFonts,
		MaxHeight:  s.cfg.UI.MaxHeight,
		MaxWidth:   s.cfg.UI.MaxWidth,
		Query:      buf,
		Selected:   s.selected,
		Footer:     s.footerHints(),
	}
	s.frame = overlay.RenderMenu(s.out, s.suggs, opts)

	// Ghost text (UI-012/013): only at end-of-line with a prefix match.
	s.ghostOn = false
	if s.cfg.UI.GhostText && s.cursor == len(s.buffer) && s.selected >= 0 && len(buf) > 0 {
		if suffix, ok := overlay.GhostSuffix(buf, s.suggs[s.selected]); ok {
			avail := s.termW - col - 1
			if avail > 0 {
				overlay.RenderGhost(s.out, suffix, avail)
				s.ghostOn = true
			}
		}
	}
}

// footerHints resolves the configured keybindings for the modern footer
// (UI-008).
func (s *sess) footerHints() []overlay.FooterHint {
	mode := "history"
	if s.mode == core.ModeHistory {
		mode = "spec"
	}
	return []overlay.FooterHint{
		{Key: s.cfg.Keybindings.Select, Action: "insert"},
		{Key: s.cfg.Keybindings.NavigateUp + "/" + s.cfg.Keybindings.NavigateDown, Action: "navigate"},
		{Key: s.cfg.Keybindings.ToggleMode, Action: mode},
		{Key: s.cfg.Keybindings.ToggleMenu, Action: "close"},
	}
}

// clearOverlay erases the frame and ghost text if present.
func (s *sess) clearOverlay() {
	s.outMu.Lock()
	s.clearOverlayLocked()
	s.outMu.Unlock()
}

func (s *sess) clearOverlayLocked() {
	if s.frame.Lines > 0 {
		overlay.ClearMenu(s.out, s.frame)
		s.frame = overlay.Frame{}
	}
	if s.ghostOn {
		overlay.ClearGhost(s.out)
		s.ghostOn = false
	}
	s.emptyPred = false
}

// clearGhost erases only the ghost text.
func (s *sess) clearGhost() {
	s.outMu.Lock()
	if s.ghostOn {
		overlay.ClearGhost(s.out)
		s.ghostOn = false
	}
	s.outMu.Unlock()
}

// clearOverlaySync is used during shutdown/reload.
func (s *sess) clearOverlaySync() { s.clearOverlay() }

// linesBelow reports the free rows under the real cursor.
func (s *sess) linesBelow() int {
	row, _ := s.vt.pos()
	below := s.termH - 1 - row
	if below < 0 {
		return 0
	}
	return below
}

// ensureSpaceLocked scrolls the terminal when fewer than n rows are free
// below the cursor, so the active prompt is never scrolled away (UI-002).
// The cursor ends above its start by the scrolled amount; the tracker is
// told explicitly since these writes bypass it.
func (s *sess) ensureSpaceLocked(n int) {
	free := s.linesBelow()
	if free >= n {
		return
	}
	k := n - free
	_, _ = s.out.Write([]byte(strings.Repeat("\n", k)))
	_, _ = fmt.Fprintf(s.out, "\x1b[%dA", k)
	s.vt.scrolledUp(k)
}

// updateCheck performs the asynchronous startup update check (UPD-002/003).
func (s *sess) updateCheck() {
	st, err := config.LoadState(s.paths.StateFile)
	if err != nil {
		return
	}
	if time.Since(st.Updater.LastCheckTime) < s.cfg.Updater.CheckInterval.D() {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	info, newer, err := updater.Check(ctx, s.cfg.Updater.Channel, s.version)
	cancel()
	if err != nil {
		logs.Debug("update check failed", "err", err)
		return
	}
	st.Updater.LastCheckTime = time.Now().UTC()
	_ = config.SaveState(s.paths.StateFile, st)
	if !newer || st.Updater.SeenVersion == info.Version {
		return
	}
	select {
	case s.updCh <- info.Version:
	default:
	}
}
