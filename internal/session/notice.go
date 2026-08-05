package session

import (
	"fmt"
	"time"

	"github.com/rselbach/argmax/internal/logging"
	"github.com/rselbach/argmax/internal/state"
	"github.com/rselbach/argmax/internal/update"
)

// watchUpdateNotice arms an in-session notification when the asynchronous
// release check has discovered a newer version. Each newly discovered
// version notifies once, and only after a command completes so the prompt
// is stable.
func (s *Session) watchUpdateNotice() {
	if s.opts.Version == "" || s.opts.Version == "dev" {
		return
	}
	// The startup check runs concurrently; look for its result a few
	// times, then stop.
	for _, delay := range []time.Duration{5 * time.Second, 30 * time.Second, 5 * time.Minute} {
		select {
		case <-s.done:
			return
		case <-time.After(delay):
		}
		st := state.Load()
		seen := st.Updater.SeenVersion
		if seen == "" || seen == st.Updater.NotifiedVersion || !update.IsNewer(s.opts.Version, seen) {
			continue
		}
		s.mu.Lock()
		s.updateNotice = fmt.Sprintf("argmax %s is available (installed %s) — run 'argmax update'", seen, s.opts.Version)
		s.mu.Unlock()
		st.Updater.NotifiedVersion = seen
		if err := state.Save(st); err != nil {
			logging.L().Debug("update notice state save failed", "error", err)
		}
		return
	}
}

// showPendingNotice displays the armed notice below a stable prompt.
// Callers invoke it from the prompt-ready path once the cursor position
// has been re-measured.
func (s *Session) showPendingNotice() {
	s.mu.Lock()
	notice := s.updateNotice
	shown := s.noticeShown
	if notice != "" {
		s.noticeShown = true
	}
	s.mu.Unlock()
	if notice == "" || shown {
		return
	}
	// Give the cursor query issued at prompt-ready time a moment to
	// resolve so the notice lands under the prompt.
	s.mu.Lock()
	if s.noticeTimer != nil {
		s.noticeTimer.Stop()
	}
	s.noticeTimer = time.AfterFunc(80*time.Millisecond, func() {
		s.launchWorker(func() {
			s.mu.Lock()
			busy := s.commandActive || s.menuVisible
			s.mu.Unlock()
			if !busy {
				s.renderer.Notice(notice)
			}
		})
	})
	s.mu.Unlock()
}
