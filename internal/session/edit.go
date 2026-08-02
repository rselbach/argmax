package session

import (
	"fmt"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
	"github.com/rselbach/argmax/internal/overlay"
)

// dispatch handles one decoded input event (PRD 9.3).
func (s *sess) dispatch(ev keyEvent) {
	// Cursor reports (and similar terminal responses) are forwarded to the
	// child: they are answers to queries issued by programs inside the pty
	// (prompt frameworks, editors), which must not be swallowed.
	if ev.kind == keyDSR {
		s.forward(ev)
		return
	}

	// While a foreground command owns the terminal, forward everything
	// (RUN-005). The Bash fallback checks the foreground process group.
	if s.commandRunning() {
		s.forward(ev)
		return
	}

	switch {
	case sameKey(ev, s.keys.toggleMenu):
		s.onToggleMenu()
	case sameKey(ev, s.keys.toggleMode):
		s.onToggleMode()
	case sameKey(ev, s.keys.select_):
		s.onSelect(ev)
	case sameKey(ev, s.keys.navUp):
		s.onNavigate(-1)
	case sameKey(ev, s.keys.navDown):
		s.onNavigate(+1)
	default:
		s.dispatchFixed(ev)
	}
}

// commandRunning reports whether a foreground command owns the terminal.
// Hook events are the primary signal; the foreground process group is the
// fallback (RUN-006) and doubles as recovery when hooks are missing or
// misconfigured, so a bare rc file degrades gracefully instead of freezing
// the UI after the first command.
func (s *sess) commandRunning() bool {
	if s.shellPgid > 0 {
		if pg, err := foregroundPgrp(int(s.ptmx.Fd())); err == nil {
			if pg != s.shellPgid {
				s.cmdActive = true
				return true
			}
			// The shell owns the terminal again. Without a preexec/prompt
			// event pair this is the only way cmdActive clears; the grace
			// window avoids flipping during the Enter-to-exec race.
			if s.cmdActive && time.Since(s.cmdStart) > 300*time.Millisecond {
				s.cmdActive = false
				s.atPrompt = true
				if !s.sawHooks {
					// Hook-less fallback session: record completion with an
					// unknown (failure) status so transitions still learn.
					s.eng.CommandFinished(-1)
				}
			}
		}
	}
	return s.cmdActive || !s.atPrompt
}

// forward passes the event's raw bytes to the child PTY unchanged.
func (s *sess) forward(ev keyEvent) {
	if ev.kind == keyUnknown && len(ev.raw) > 0 {
		s.writePty(ev.raw)
		return
	}
	s.writePty(ev.rawBytes())
}

// rawBytes reconstructs the byte form of a decoded event for forwarding.
func (ev keyEvent) rawBytes() []byte {
	switch ev.kind {
	case keyRune:
		return []byte(string(ev.r))
	case keyCtrl:
		if ev.ctrl == 0 {
			return []byte{0}
		}
		return []byte{ev.ctrl - 'a' + 1}
	case keyBackspace:
		return []byte{0x7f}
	case keySpecial:
		switch ev.spec {
		case config.KeyEnter:
			return []byte{'\r'}
		case config.KeyTab:
			return []byte{'\t'}
		case config.KeyShiftTab:
			return []byte("\x1b[Z")
		case config.KeyUp:
			return []byte("\x1b[A")
		case config.KeyDown:
			return []byte("\x1b[B")
		case config.KeyRight:
			return []byte("\x1b[C")
		case config.KeyLeft:
			return []byte("\x1b[D")
		}
	case keyEscape:
		return []byte{0x1b}
	case keyDSR:
		return []byte(fmt.Sprintf("\x1b[%d;%dR", ev.rows, ev.cols))
	case keyPasteStart:
		return []byte("\x1b[200~")
	case keyPasteEnd:
		return []byte("\x1b[201~")
	}
	return nil
}

func (s *sess) writePty(b []byte) {
	if len(b) == 0 {
		return
	}
	_, _ = s.ptmx.Write(b)
}

// dispatchFixed handles the fixed (non-configurable) key behaviors (PRD 9.3).
func (s *sess) dispatchFixed(ev keyEvent) {
	switch ev.kind {
	case keyRune:
		s.insertRune(ev.r)
	case keyCtrl:
		s.onCtrl(ev.ctrl)
	case keyBackspace:
		s.onBackspace()
	case keySpecial:
		s.onSpecial(ev.spec)
	case keyEscape:
		s.onEscape()
	case keyPasteStart:
		s.pasting = true
		s.pasteBuf = s.pasteBuf[:0]
		s.writePty(ev.rawBytes())
	case keyPasteEnd:
		s.pasting = false
		s.writePty(ev.rawBytes())
		s.buffer = insertRunes(s.buffer, s.cursor, s.pasteBuf)
		s.cursor += len(s.pasteBuf)
		s.bufferGen++
		s.scheduleRender()
	case keyUnknown:
		// IN-013: preserve unknown escape sequences intact.
		s.writePty(ev.raw)
	}
}

// insertRune handles a printable character (or a space triggering alias
// expansion, SRC-005).
func (s *sess) insertRune(r rune) {
	if s.pasting {
		s.pasteBuf = append(s.pasteBuf, r)
		s.writePty([]byte(string(r)))
		return
	}
	s.clearOverlay()

	if r == ' ' && s.cursor == len(s.buffer) {
		if exp, ok := s.eng.ExpandAlias(string(s.buffer)); ok {
			// Replace the alias with its expansion in the shell buffer.
			s.replaceLine(exp)
			s.writePty([]byte(" "))
			s.buffer = []rune(exp + " ")
			s.cursor = len(s.buffer)
			s.bufferGen++
			s.scheduleRender()
			return
		}
	}

	s.writePty([]byte(string(r)))
	s.buffer = insertRunes(s.buffer, s.cursor, []rune{r})
	s.cursor++
	s.bufferGen++
	s.hiddenEdit = false
	s.scheduleRender()
	s.scheduleAI()
}

func insertRunes(buf []rune, at int, rs []rune) []rune {
	out := make([]rune, 0, len(buf)+len(rs))
	out = append(out, buf[:at]...)
	out = append(out, rs...)
	out = append(out, buf[at:]...)
	return out
}

// onBackspace removes the character before the cursor (IN-003).
func (s *sess) onBackspace() {
	if s.pasting {
		s.writePty([]byte{0x7f})
		return
	}
	s.clearOverlay()
	s.writePty([]byte{0x7f})
	if s.cursor > 0 {
		s.buffer = append(s.buffer[:s.cursor-1], s.buffer[s.cursor:]...)
		s.cursor--
	}
	s.bufferGen++
	if len(s.buffer) == 0 {
		s.menuOpen = false // close when the buffer becomes empty (IN-003)
	}
	s.scheduleRender()
}

// onCtrl handles raw control bytes (IN-005, IN-014).
func (s *sess) onCtrl(c byte) {
	s.clearOverlay()
	s.writePty([]byte{ctrlByte(c)})
	switch c {
	case 'a': // beginning of line (IN-004)
		s.cursor = 0
	case 'e': // end of line (IN-004)
		s.cursor = len(s.buffer)
	case 'w': // delete word before cursor (IN-005)
		s.buffer, s.cursor = deleteWord(s.buffer, s.cursor)
		s.bufferGen++
	case 'u': // clear line (IN-005)
		s.buffer = s.buffer[:0]
		s.cursor = 0
		s.menuOpen = false
		s.bufferGen++
	case 'c': // cancel line (IN-005)
		s.buffer = s.buffer[:0]
		s.cursor = 0
		s.menuOpen = false
		s.bufferGen++
	case 'l': // clear screen, redraw (IN-005)
		s.scheduleRender()
	}
	// Ghost text visibility follows cursor-at-EOL; recompute on render.
}

func ctrlByte(c byte) byte {
	if c == 0 {
		return 0
	}
	return c - 'a' + 1
}

// deleteWord removes the whitespace and word immediately before cursor,
// returning the new buffer and cursor position.
func deleteWord(buf []rune, cursor int) ([]rune, int) {
	if cursor > len(buf) {
		cursor = len(buf)
	}
	i := cursor
	for i > 0 && buf[i-1] == ' ' {
		i--
	}
	for i > 0 && buf[i-1] != ' ' {
		i--
	}
	out := make([]rune, 0, len(buf)-(cursor-i))
	out = append(out, buf[:i]...)
	out = append(out, buf[cursor:]...)
	return out, i
}

// onSpecial handles named keys.
func (s *sess) onSpecial(spec string) {
	switch spec {
	case config.KeyEnter:
		s.onEnter()
	case config.KeyLeft:
		s.writePty([]byte("\x1b[D"))
		if s.cursor > 0 {
			s.cursor--
		}
		s.clearGhost() // ghost hidden away from end-of-line (IN-006)
	case config.KeyRight:
		s.onRightArrow()
	case config.KeyTab, config.KeyShiftTab, config.KeyUp, config.KeyDown:
		// configurable keys are dispatched earlier; forward stray ones
		s.forward(keyEvent{kind: keySpecial, spec: spec})
	}
}

// onRightArrow accepts the ghost suffix at end-of-line, else moves (IN-006).
func (s *sess) onRightArrow() {
	if s.cursor == len(s.buffer) && s.emptyPred && len(s.suggs) > 0 {
		s.acceptPrediction()
		return
	}
	if s.cursor == len(s.buffer) && s.ghostOn && len(s.suggs) > 0 && s.selected >= 0 {
		if suffix, ok := overlay.GhostSuffix(string(s.buffer), s.suggs[s.selected]); ok && suffix != "" {
			s.clearOverlay()
			s.writePty([]byte(suffix))
			s.buffer = append(s.buffer, []rune(suffix)...)
			s.cursor = len(s.buffer)
			s.bufferGen++
			s.scheduleRender()
			return
		}
	}
	s.writePty([]byte("\x1b[C"))
	if s.cursor < len(s.buffer) {
		s.cursor++
	}
}

// onEnter submits the current buffer. Enter always passes through; only Tab
// inserts a suggestion (Right Arrow accepts the ghost suffix). In history
// mode the buffer may already contain a previewed entry — that is what gets
// submitted.
func (s *sess) onEnter() {
	s.clearOverlay()
	submit := string(s.buffer)
	s.writePty([]byte{'\r'})
	s.lastSubmitted = submit
	s.eng.CommandStarted(submit) // Bash has no preexec hook; others dedupe
	s.buffer = s.buffer[:0]
	s.cursor = 0
	s.menuOpen = false
	s.atPrompt = false
	s.cmdActive = true
	s.cmdStart = time.Now()
	s.bufferGen++
}

// onEscape hides the menu and ghost text until the next eligible edit
// (IN-010) without canceling the shell line.
func (s *sess) onEscape() {
	if s.menuOpen || s.ghostOn {
		s.clearOverlay()
		s.menuOpen = false
		s.explicit = false
		s.hiddenEdit = true
		return
	}
	s.writePty([]byte{0x1b})
}

// onSelect inserts the highlighted candidate (IN-008).
func (s *sess) onSelect(ev keyEvent) {
	if !s.menuOpen || s.selected < 0 || s.selected >= len(s.suggs) {
		if s.emptyPred && len(s.suggs) > 0 {
			s.acceptPrediction()
			return
		}
		// No menu: let the shell handle the key natively.
		s.forward(ev)
		return
	}
	s.clearOverlay()
	text := s.suggs[s.selected].Text
	s.replaceLine(text)
	if !strings.HasSuffix(text, "/") {
		s.writePty([]byte(" "))
		s.buffer = []rune(text + " ")
	} else {
		s.buffer = []rune(text)
	}
	s.cursor = len(s.buffer)
	s.bufferGen++
	s.navigated = false
	s.scheduleRender()
}

// acceptPrediction inserts the displayed empty-prompt prediction.
func (s *sess) acceptPrediction() {
	text := s.suggs[0].Text
	s.clearOverlay()
	s.replaceLine(text)
	s.buffer = []rune(text)
	s.cursor = len(s.buffer)
	s.bufferGen++
	s.scheduleRender()
}

// onNavigate moves the selection or opens the candidate source (IN-007).
func (s *sess) onNavigate(delta int) {
	if !s.menuOpen {
		// Open: empty buffer → recent history; otherwise active-mode candidates.
		s.requestSuggest(true)
		return
	}
	if len(s.suggs) == 0 {
		return
	}
	s.selected += delta
	if s.selected < 0 {
		s.selected = 0
	}
	if s.selected >= len(s.suggs) {
		s.selected = len(s.suggs) - 1
	}
	s.navigated = true
	// History mode previews the full command in the prompt (IN-007).
	if s.mode == core.ModeHistory && s.selected >= 0 {
		s.preview(s.suggs[s.selected].Text)
	}
	s.draw()
}

// preview replaces the shell buffer for history preview without touching the
// suggestion state. Stale suggestion results are dropped via bufferGen.
func (s *sess) preview(text string) {
	s.replaceLine(text)
	s.buffer = []rune(text)
	s.cursor = len(s.buffer)
	s.bufferGen++
}

// onToggleMenu disables or re-enables suggestions for the session (IN-011).
func (s *sess) onToggleMenu() {
	s.menuOn = !s.menuOn
	if !s.menuOn {
		s.clearOverlay()
		s.menuOpen = false
		return
	}
	s.requestSuggest(false)
}

// onToggleMode switches spec/history mode (IN-012).
func (s *sess) onToggleMode() {
	if s.mode == core.ModeSpec {
		s.mode = core.ModeHistory
	} else {
		s.mode = core.ModeSpec
	}
	s.eng.SetMode(s.mode)
	if s.startupModeIsLast {
		st := *s.state
		st.LastMode = s.mode.String()
		go func() { _ = config.SaveState(s.paths.StateFile, &st) }()
	}
	s.menuOpen = false
	s.requestSuggest(false)
}

// replaceLine erases the current shell line and writes text (shell-adapter
// line replacement).
func (s *sess) replaceLine(text string) {
	s.writePty([]byte{0x15}) // Ctrl+U kills to line start (cursor is at EOL)
	s.writePty([]byte(text))
}

// scheduleRender coalesces suggestion renders (IN-002).
func (s *sess) scheduleRender() {
	if s.renderT != nil {
		s.renderT.Reset(20 * time.Millisecond)
	}
}

// scheduleAI resets the AI debounce timer (AI-005).
func (s *sess) scheduleAI() {
	if s.aiT != nil && s.cfg.AI.Enabled {
		s.aiT.Reset(time.Duration(orDefault(s.cfg.AI.DebounceMs, 500)) * time.Millisecond)
	}
}
