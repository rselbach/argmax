package session

import (
	"bufio"
	"context"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/logging"
)

// eventPump reads NUL-delimited shell hook records from the inherited
// session-private descriptor: buf:<lbuffer>, cwd:<path>, pre:<command>,
// post:<status>, and ready:.
func (s *Session) eventPump() {
	scanner := bufio.NewScanner(s.eventsR)
	scanner.Buffer(make([]byte, 64*1024), 64*1024)
	scanner.Split(scanNul)
	for scanner.Scan() {
		record := scanner.Text()
		s.mu.Lock()
		s.hooksSeen = true
		s.mu.Unlock()
		kind, payload, _ := strings.Cut(record, ":")
		switch kind {
		case "buf":
			s.onBuffer(payload)
		case "cwd":
			s.onCWD(payload)
		case "pre":
			s.onCommandStart(payload)
		case "post":
			s.onCommandEnd(payload)
		case "ready":
			s.onPromptReady()
		}
		select {
		case <-s.done:
			return
		default:
		}
	}
}

func scanNul(data []byte, atEOF bool) (advance int, token []byte, err error) {
	for i, b := range data {
		if b == 0 {
			return i + 1, data[:i], nil
		}
	}
	if atEOF && len(data) > 0 {
		return len(data), data, nil
	}
	return 0, nil, nil
}

// onBuffer resyncs the tracked line from a shell buffer report.
func (s *Session) onBuffer(lbuffer string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.commandActive || s.pasting {
		return
	}
	if s.buf.String() != lbuffer {
		s.buf.Set(lbuffer)
		s.scheduleComputeLocked()
	}
}

// onCWD tracks the child shell's working directory, rejecting relative
// updates.
func (s *Session) onCWD(path string) {
	if !filepath.IsAbs(path) {
		return
	}
	s.mu.Lock()
	s.cwd = path
	s.mu.Unlock()
}

// onCommandStart hides the overlay and enters transparent passthrough.
func (s *Session) onCommandStart(command string) {
	s.mu.Lock()
	s.commandActive = true
	s.menuVisible = false
	if s.lastSubmitted == "" {
		s.lastSubmitted = strings.TrimSpace(command)
		if s.lastSubmitted != "" {
			s.hist.AddSession(s.lastSubmitted)
		}
	}
	s.mu.Unlock()
	s.renderer.Clear()
	s.aiEngine.Cancel()
}

// onCommandEnd records the completed command with its exit status for
// frecency and transition learning.
func (s *Session) onCommandEnd(payload string) {
	status, err := strconv.Atoi(strings.TrimSpace(payload))
	if err != nil {
		return
	}
	s.mu.Lock()
	command := s.lastSubmitted
	s.lastSubmitted = ""
	prevSkeleton := s.prevSkeleton
	cwd := s.cwd
	s.prevExit = status
	if command != "" {
		s.prevCommand = command
		s.prevSkeleton = s.registry.Skeleton(command)
	}
	s.mu.Unlock()
	if command == "" {
		return
	}
	skeleton := s.registry.Skeleton(command)
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
		defer cancel()
		if err := s.store.Record(ctx, command, skeleton, prevSkeleton, cwd, status); err != nil {
			logging.L().Debug("usage recording failed", "error", err)
		}
	}()
}

// onPromptReady returns to prompt-tracking state.
func (s *Session) onPromptReady() {
	s.mu.Lock()
	s.commandActive = false
	s.buf.Clear()
	s.menuVisible = false
	s.suppressed = false
	s.navigated = false
	s.items = nil
	s.selected = -1
	s.scroll = 0
	s.mu.Unlock()
	s.requestCursor()
	s.showPendingNotice()
	s.maybePredictEmpty()
}
