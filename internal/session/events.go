package session

import (
	"bufio"
	"context"
	"errors"
	"io"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/logging"
)

const maxEventRecordSize = 64 * 1024

// eventPump reads NUL-delimited shell hook records from the inherited
// session-private descriptor: buf:<lbuffer>, cwd:<path>, pre:<command>,
// post:<status>, and ready:.
func (s *Session) eventPump() {
	err := readEventRecords(s.eventsR, func(record string) bool {
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
			return false
		default:
			return true
		}
	}, func() {
		logging.L().Warn("oversized shell integration event discarded", "max_bytes", maxEventRecordSize)
	})
	if errors.Is(err, io.EOF) {
		// Pipe EOF races the normal child-exit path. Give serve a moment to
		// close done before treating it as an integration failure.
		select {
		case <-s.done:
			return
		case <-time.After(10 * time.Millisecond):
		}
	}
	select {
	case <-s.done:
		return
	default:
	}
	s.mu.Lock()
	hadHooks := s.hooksSeen
	s.hooksSeen = false
	s.mu.Unlock()
	if err != nil && !errors.Is(err, io.EOF) {
		logging.L().Warn("shell integration event stream failed; using foreground fallback", "error", err)
	} else if hadHooks {
		logging.L().Warn("shell integration event stream closed; using foreground fallback")
	}
}

// readEventRecords reads bounded NUL-delimited records. An oversized record
// is discarded through its next delimiter without losing subsequent events.
func readEventRecords(r io.Reader, onRecord func(string) bool, onOversized func()) error {
	reader := bufio.NewReaderSize(r, 32*1024)
	record := make([]byte, 0, 1024)
	discarding := false
	for {
		fragment, err := reader.ReadSlice(0)
		terminated := err == nil
		if terminated {
			fragment = fragment[:len(fragment)-1]
		}
		if !discarding {
			if len(record)+len(fragment) > maxEventRecordSize {
				record = record[:0]
				discarding = true
				onOversized()
			} else {
				record = append(record, fragment...)
			}
		}
		if terminated {
			if !discarding {
				if !onRecord(string(record)) {
					return nil
				}
			}
			record = record[:0]
			discarding = false
			continue
		}
		if errors.Is(err, bufio.ErrBufferFull) {
			continue
		}
		if errors.Is(err, io.EOF) {
			if len(record) > 0 && !discarding {
				onRecord(string(record))
			}
			return io.EOF
		}
		return err
	}
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
