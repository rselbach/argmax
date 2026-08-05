package session

import (
	"strings"

	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/input"
	"github.com/rselbach/argmax/internal/keymap"
	"github.com/rselbach/argmax/internal/logging"
	"github.com/rselbach/argmax/internal/state"
	"github.com/rselbach/argmax/internal/ui"
)

// forward writes raw bytes to the child PTY.
func (s *Session) forward(p []byte) {
	if _, err := s.ptmx.Write(p); err != nil {
		logging.L().Debug("pty write failed", "error", err)
	}
}

// handleEvent processes one decoded input event.
func (s *Session) handleEvent(ev input.Event) {
	if ev.Kind == input.EventCPR {
		s.mu.Lock()
		if s.dsrOutstanding == 0 {
			// Not our query: a prompt theme or TUI asked for the cursor
			// position and is reading the reply from its own input.
			s.mu.Unlock()
			s.forward(ev.Raw)
			return
		}
		s.dsrOutstanding--
		s.mu.Unlock()
		s.renderer.SetCursor(ev.Row, ev.Col)
		s.mu.Lock()
		if s.renderPending && s.renderer.OutputSeq() != s.dsrSeq && s.dsrRetries < 5 {
			// Shell output (such as a keystroke echo) raced the position
			// query, so the report may predate it; measure again.
			s.dsrRetries++
			s.mu.Unlock()
			s.requestCursor()
			return
		}
		pending := s.renderPending && s.renderSeq == s.buf.Version()
		s.renderPending = false
		s.mu.Unlock()
		if pending {
			// The report is current: no edits or output arrived since the
			// query, so drawing (and erasing) at the position is safe. A
			// stale report is dropped; the pending recompute re-queries.
			s.render()
		}
		return
	}

	s.mu.Lock()
	passthrough := s.commandActive
	pasting := s.pasting
	s.mu.Unlock()

	if passthrough {
		s.forward(ev.Raw)
		return
	}

	switch ev.Kind {
	case input.EventPasteStart:
		s.mu.Lock()
		s.pasting = true
		s.mu.Unlock()
		s.forward(ev.Raw)
		return
	case input.EventPasteEnd:
		s.mu.Lock()
		s.pasting = false
		s.mu.Unlock()
		s.forward(ev.Raw)
		s.scheduleCompute()
		return
	case input.EventRaw:
		// Preserve unknown escape sequences intact.
		s.forward(ev.Raw)
		return
	}

	if pasting {
		// Forward pasted bytes unchanged, without alias expansion, while
		// keeping a best-effort buffer model.
		s.forward(ev.Raw)
		s.mu.Lock()
		if ev.Key.Kind == keymap.KindRune {
			s.buf.Insert(ev.Key.Rune)
		} else {
			s.buf.Clear() // control data inside a paste desyncs the model
		}
		s.mu.Unlock()
		return
	}

	s.handleKey(ev)
}

// handleKey implements the prompt-time key behavior.
func (s *Session) handleKey(ev input.Event) {
	key := ev.Key
	s.mu.Lock()
	keys := s.keys
	menuOpen := s.menuVisible && s.menuEnabled
	s.mu.Unlock()

	// Enter is reserved: it always submits, regardless of configured
	// bindings.
	if key.Kind == keymap.KindEnter {
		s.handleEnter(ev)
		return
	}
	switch key {
	case keys.toggleMenu:
		s.toggleMenu()
		return
	case keys.toggleMode:
		s.toggleMode()
		return
	case keys.navUp:
		s.navigate(-1, ev)
		return
	case keys.navDown:
		s.navigate(1, ev)
		return
	case keys.selectKey:
		if menuOpen {
			s.acceptSelected()
			return
		}
		// Fall back to the shell's native behavior for the key.
		s.forward(ev.Raw)
		return
	}

	switch key.Kind {
	case keymap.KindRune:
		s.handleRune(ev)
	case keymap.KindBackspace:
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.Backspace()
		empty := s.buf.Empty()
		if empty {
			s.menuVisible = false
			s.items = nil
		}
		s.suppressed = false
		s.navigated = false
		s.scheduleComputeLocked()
		s.mu.Unlock()
		if empty {
			s.hideOverlay()
		}
	case keymap.KindLeft:
		s.clearGhostBeforeCursorMove()
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveLeft()
		s.mu.Unlock()
		s.aiEngine.Cancel()
	case keymap.KindRight:
		if s.acceptGhostSuffix() {
			return
		}
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveRight()
		s.mu.Unlock()
	case keymap.KindHome:
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveHome()
		s.mu.Unlock()
	case keymap.KindEnd:
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveEnd()
		s.mu.Unlock()
	case keymap.KindEscape:
		s.mu.Lock()
		s.suppressed = true
		s.menuVisible = false
		s.mu.Unlock()
		s.hideOverlay()
	case keymap.KindCtrl:
		s.handleCtrl(ev)
	case keymap.KindDelete, keymap.KindTab, keymap.KindShiftTab, keymap.KindCtrlSpace:
		s.forward(ev.Raw)
	default:
		s.forward(ev.Raw)
	}
}

func (s *Session) handleRune(ev input.Event) {
	r := ev.Key.Rune
	if r == ' ' && s.maybeExpandAlias() {
		return
	}
	s.forward(ev.Raw)
	s.mu.Lock()
	atEnd := s.buf.AtEnd()
	s.buf.Insert(r)
	s.suppressed = false
	s.navigated = false
	s.scheduleComputeLocked()
	s.mu.Unlock()
	if atEnd {
		// The echo lands on the ghost's first cell.
		s.renderer.ShrinkGhost(ui.VisibleWidth(string(r)))
	}
}

// maybeExpandAlias replaces a single typed shell alias with its expansion
// when core.expand-alias is enabled. Returns true when handled.
func (s *Session) maybeExpandAlias() bool {
	cfg := s.opts.Watcher.Current()
	if !cfg.Core.ExpandAlias {
		return false
	}
	s.mu.Lock()
	line := s.buf.String()
	atEnd := s.buf.AtEnd()
	s.mu.Unlock()
	if !atEnd || line == "" || strings.ContainsAny(line, " \t") {
		return false
	}
	for _, a := range s.aliases.Aliases() {
		if a.Name != line {
			continue
		}
		// Kill the typed alias word, then write the expansion. The
		// rewrite covers any ghost cells; forget them first.
		s.renderer.AcceptGhost()
		s.forward([]byte{0x17}) // Ctrl+W
		s.forward([]byte(a.Expansion + " "))
		s.mu.Lock()
		s.buf.Set(a.Expansion + " ")
		s.scheduleComputeLocked()
		s.mu.Unlock()
		return true
	}
	return false
}

func (s *Session) handleCtrl(ev input.Event) {
	switch ev.Key.Rune {
	case 'a':
		s.clearGhostBeforeCursorMove()
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveHome()
		s.mu.Unlock()
	case 'e':
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.MoveEnd()
		s.mu.Unlock()
	case 'w':
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.DeleteWordBack()
		s.scheduleComputeLocked()
		s.mu.Unlock()
	case 'u':
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.Clear()
		s.menuVisible = false
		s.items = nil
		s.mu.Unlock()
		s.hideOverlay()
	case 'c':
		s.forward(ev.Raw)
		s.mu.Lock()
		s.buf.Clear()
		s.menuVisible = false
		s.navigated = false
		s.items = nil
		s.mu.Unlock()
		s.hideOverlay()
		s.aiEngine.Cancel()
	case 'l':
		s.renderer.Clear()
		s.forward(ev.Raw)
		// The shell repaints the prompt; re-measure and redraw the menu.
		if s.menuOpenSnapshot() {
			s.queueRender()
		}
	default:
		s.forward(ev.Raw)
	}
}

// handleEnter submits the current line, or a deliberately selected
// candidate after menu navigation.
func (s *Session) handleEnter(ev input.Event) {
	cfg := s.opts.Watcher.Current()
	s.mu.Lock()
	var chosen *complete.Candidate
	if s.menuVisible && s.selected >= 0 && s.selected < len(s.items) {
		if s.navigated || cfg.Core.AutoExecute {
			c := s.items[s.selected]
			chosen = &c
		}
	}
	s.menuVisible = false
	s.items = nil
	s.mu.Unlock()
	s.hideOverlay()
	s.aiEngine.Cancel()

	if chosen != nil {
		s.replaceLine(chosen.Text)
	}
	s.mu.Lock()
	submitted := strings.TrimSpace(s.buf.String())
	if submitted != "" && s.lastSubmitted == "" {
		s.lastSubmitted = submitted
	}
	s.commandActive = true // until the shell reports prompt-ready
	s.buf.Clear()
	s.mu.Unlock()
	if submitted != "" {
		s.hist.AddSession(submitted)
	}
	s.forward([]byte{'\r'})
}

// replaceLine rewrites the shell's editable line with text.
func (s *Session) replaceLine(text string) {
	// The rewrite echoes into cells the ghost may occupy; forget them
	// first so a concurrent overlay Clear cannot blank the real text.
	s.renderer.AcceptGhost()
	// Ctrl+E then Ctrl+U clears the whole line from any cursor position.
	s.forward([]byte{0x05, 0x15})
	s.forward([]byte(text))
	s.mu.Lock()
	s.buf.Set(text)
	s.mu.Unlock()
}

// acceptSelected inserts the highlighted candidate, appending a space
// except after a directory suffix so path traversal can continue.
func (s *Session) acceptSelected() {
	s.mu.Lock()
	if s.selected < 0 || s.selected >= len(s.items) {
		s.mu.Unlock()
		return
	}
	c := s.items[s.selected]
	s.mu.Unlock()
	text := c.Text
	if !c.IsDirectory && !strings.HasSuffix(text, " ") {
		text += " "
	}
	s.replaceLine(text)
	s.mu.Lock()
	s.navigated = false
	s.scheduleComputeLocked()
	s.mu.Unlock()
}

// acceptGhostSuffix accepts the visible ghost text on Right Arrow at
// end-of-line. Returns true when handled.
func (s *Session) acceptGhostSuffix() bool {
	s.mu.Lock()
	ghost := s.currentGhostLocked()
	atEnd := s.buf.AtEnd()
	if ghost == "" || !atEnd {
		s.mu.Unlock()
		return false
	}
	s.buf.InsertString(ghost)
	s.scheduleComputeLocked()
	s.mu.Unlock()
	// The shell echoes the suffix into the drawn ghost cells; keep them.
	s.renderer.AcceptGhost()
	s.forward([]byte(ghost))
	return true
}

// navigate moves the menu selection, opening the menu when closed.
func (s *Session) navigate(delta int, ev input.Event) {
	s.mu.Lock()
	if !s.menuEnabled {
		s.mu.Unlock()
		s.forward(ev.Raw)
		return
	}
	if !s.menuVisible {
		// Opening: an empty buffer opens recent history; a non-empty
		// buffer opens candidates for the active mode.
		openMode := s.mode
		if s.buf.Empty() {
			openMode = modeHistory
		}
		s.suppressed = false
		s.mu.Unlock()
		s.openMenu(openMode)
		return
	}
	if len(s.items) == 0 {
		s.mu.Unlock()
		return
	}
	s.navigated = true
	s.selected += delta
	if s.selected < 0 {
		s.selected = len(s.items) - 1
	}
	if s.selected >= len(s.items) {
		s.selected = 0
	}
	s.keepSelectionVisibleLocked()
	previewMode := s.mode
	preview := ""
	if previewMode == modeHistory && s.selected < len(s.items) {
		preview = s.items[s.selected].Text
	}
	s.mu.Unlock()
	s.aiEngine.Cancel()
	if preview != "" {
		// History selection previews the full command in the shell buffer.
		s.replaceLine(preview)
	}
	s.render()
}

func (s *Session) keepSelectionVisibleLocked() {
	cfg := s.opts.Watcher.Current()
	window := cfg.UI.MaxHeight
	if window < 1 {
		window = 15
	}
	if s.selected >= 0 {
		if s.selected < s.scroll {
			s.scroll = s.selected
		}
		if s.selected >= s.scroll+window {
			s.scroll = s.selected - window + 1
		}
	}
}

// toggleMenu disables or re-enables prompt suggestions for the session.
func (s *Session) toggleMenu() {
	s.mu.Lock()
	s.menuEnabled = !s.menuEnabled
	if !s.menuEnabled {
		s.menuVisible = false
	}
	enabled := s.menuEnabled
	s.mu.Unlock()
	if !enabled {
		s.hideOverlay()
		return
	}
	s.scheduleCompute()
}

// toggleMode switches spec/history mode, persisting the selection when the
// startup mode is "last".
func (s *Session) toggleMode() {
	s.mu.Lock()
	if s.mode == modeSpec {
		s.mode = modeHistory
	} else {
		s.mode = modeSpec
	}
	current := s.mode
	s.menuVisible = true
	s.mu.Unlock()
	if s.opts.Watcher.Current().Core.Mode == "last" {
		if err := state.Update(func(st *state.State) {
			st.LastMode = string(current)
		}); err != nil {
			logging.L().Debug("state save failed", "error", err)
		}
	}
	s.scheduleCompute()
}

// openMenu shows candidates for the given mode.
func (s *Session) openMenu(m mode) {
	s.mu.Lock()
	s.menuVisible = true
	s.mu.Unlock()
	s.computeIn(m)
}

// clearGhostBeforeCursorMove erases ghost text before the shell cursor
// leaves end-of-line, so no stale cells remain.
func (s *Session) clearGhostBeforeCursorMove() {
	s.renderer.Clear()
	if s.menuOpenSnapshot() {
		s.queueRender()
	}
}

func (s *Session) menuOpenSnapshot() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.menuVisible
}

// hideOverlay clears menu and ghost from the screen.
func (s *Session) hideOverlay() {
	s.renderer.Clear()
}
