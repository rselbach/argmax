package session

import (
	"context"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/ai"
	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/history"
	"github.com/rselbach/argmax/internal/logging"
	"github.com/rselbach/argmax/internal/rank"
)

const (
	// coalesceWindow batches bursts of edits before recomputing.
	coalesceWindow = 20 * time.Millisecond
	// signalBudget bounds context and database signal collection.
	signalBudget = 500 * time.Millisecond
)

// scheduleCompute recomputes suggestions after the coalescing window.
func (s *Session) scheduleCompute() {
	s.mu.Lock()
	s.scheduleComputeLocked()
	s.mu.Unlock()
}

func (s *Session) scheduleComputeLocked() {
	if s.coalesce != nil {
		s.coalesce.Stop()
	}
	s.coalesce = time.AfterFunc(coalesceWindow, s.compute)
}

func (s *Session) compute() {
	s.mu.Lock()
	m := s.mode
	s.mu.Unlock()
	s.computeIn(m)
}

// computeIn recomputes candidates for the given mode in the background and
// applies them if the buffer has not changed meanwhile.
func (s *Session) computeIn(m mode) {
	s.mu.Lock()
	if s.commandActive || !s.menuEnabled || s.suppressed {
		s.mu.Unlock()
		return
	}
	s.generation++
	gen := s.generation
	line := s.buf.String()
	cwd := s.cwd
	explicitOpen := s.menuVisible
	atEnd := s.buf.AtEnd()
	navigated := s.navigated
	s.mu.Unlock()

	if strings.TrimSpace(line) == "" && !explicitOpen {
		s.mu.Lock()
		s.items = nil
		s.menuVisible = false
		s.mu.Unlock()
		s.hideOverlay()
		return
	}

	s.launchWorker(func() {
		cfg := s.opts.Watcher.Current()
		cands := s.candidatesFor(m, line, cwd, cfg)
		cands = complete.Dedupe(cands, line)
		if m == modeSpec {
			cands = s.rankSpec(cands, line, cwd)
		}
		if len(cands) > cfg.UI.MaxSuggestions {
			cands = cands[:cfg.UI.MaxSuggestions]
		}
		s.apply(gen, cands)
		if m == modeSpec && atEnd && !navigated && cfg.AI.Enabled {
			s.requestAI(line, cwd)
		}
	})
}

// queueRender schedules a draw at the next trustworthy cursor report.
func (s *Session) queueRender() {
	s.mu.Lock()
	s.renderPending = true
	s.renderSeq = s.buf.Version()
	s.dsrRetries = 0
	s.mu.Unlock()
	s.requestCursor()
}

// requestCursor issues one owned cursor query, recording both the output
// sequence for staleness detection and the outstanding count for reply
// ownership.
func (s *Session) requestCursor() {
	s.mu.Lock()
	s.dsrOutstanding++
	s.mu.Unlock()
	seq := s.renderer.RequestCursor()
	s.mu.Lock()
	s.dsrSeq = seq
	s.mu.Unlock()
}

// apply installs computed candidates when the generation is still current.
func (s *Session) apply(gen uint64, cands []complete.Candidate) {
	s.mu.Lock()
	if gen != s.generation || s.commandActive {
		s.mu.Unlock()
		return
	}
	s.items = cands
	if !s.navigated || s.selected >= len(cands) {
		s.selected = 0
		s.scroll = 0
	}
	if len(cands) == 0 {
		s.selected = -1
	}
	s.menuVisible = len(cands) > 0
	visible := s.menuVisible
	s.mu.Unlock()
	if !visible {
		s.hideOverlay()
	}
	s.queueRender()
}

// render draws the current menu and ghost state.
func (s *Session) render() {
	s.mu.Lock()
	if s.commandActive || (!s.menuVisible && s.currentGhostLocked() == "") {
		s.mu.Unlock()
		s.hideOverlay()
		return
	}
	items := s.items
	selected := s.selected
	scroll := s.scroll
	query := s.buf.String()
	ghost := s.currentGhostLocked()
	visible := s.menuVisible
	s.mu.Unlock()
	if !visible {
		items = nil
	}
	s.renderer.Render(items, selected, scroll, query, ghost)
}

// currentGhostLocked returns the untyped suffix of the selected candidate,
// shown only for a non-empty case-insensitive prefix at end-of-line.
func (s *Session) currentGhostLocked() string {
	if !s.opts.Watcher.Current().UI.GhostText {
		return ""
	}
	if s.buf.IsBlank() || !s.buf.AtEnd() || s.suppressed {
		return ""
	}
	idx := s.selected
	if idx < 0 {
		idx = 0
	}
	if idx >= len(s.items) {
		return ""
	}
	text := s.items[idx].Text
	line := s.buf.String()
	if len(text) <= len(line) || !strings.EqualFold(text[:len(line)], line) {
		return ""
	}
	return text[len(line):]
}

// rankSpec applies the weighted adaptive ranking with a bounded signal
// collection budget.
func (s *Session) rankSpec(cands []complete.Candidate, line, cwd string) []complete.Candidate {
	parent := s.ctx
	if parent == nil {
		parent = context.Background()
	}
	ctx, cancel := context.WithTimeout(parent, signalBudget)
	defer cancel()
	sig := rank.Signals{
		Workspace: s.detector.Detect(cwd),
		Skeleton:  s.registry.Skeleton,
	}
	if fr, err := s.store.Frecency(ctx, cwd); err == nil {
		sig.Frecency = fr
	} else {
		logging.L().Debug("frecency unavailable", "error", err)
	}
	s.mu.Lock()
	prevSkeleton := s.prevSkeleton
	s.mu.Unlock()
	if tr, err := s.store.Transitions(ctx, prevSkeleton, cwd, complete.ParentSkeleton); err == nil {
		sig.Transitions = tr
	} else {
		logging.L().Debug("transitions unavailable", "error", err)
	}
	return rank.Rank(cands, line, sig)
}

// candidatesFor merges the candidate sources for the buffer.
func (s *Session) candidatesFor(m mode, line, cwd string, cfg *config.Config) []complete.Candidate {
	if m == modeHistory {
		return s.historyCandidates(line, cwd, cfg)
	}
	return s.specCandidates(line, cwd, cfg)
}

// specCandidates implements spec mode: aliases, bundled specs, PATH
// executables, dynamic values, inferred specs.
func (s *Session) specCandidates(line, cwd string, cfg *config.Config) []complete.Candidate {
	ctx := complete.Context{
		CWD:                    cwd,
		Shell:                  s.opts.Shell,
		HiddenFiles:            cfg.UI.HiddenFiles,
		GitFilterActiveBranch:  cfg.Git.FilterActiveBranch,
		GitDeduplicateBranches: cfg.Git.DeduplicateBranches,
	}
	tokens := complete.Tokenize(line)
	if len(tokens) <= 1 {
		return s.topLevel(tokens[0].Text)
	}

	root := tokens[0].Text
	cands := s.engine.Complete(ctx, line)
	if s.registry.Lookup(root) == nil {
		if expanded := s.expandRootAlias(ctx, line, root); expanded != nil {
			cands = append(cands, expanded...)
		} else {
			cands = append(cands, s.inferred(ctx, line, tokens)...)
		}
	}
	cands = append(cands, s.toolAliasCandidates(line, cwd, tokens)...)
	return cands
}

// expandRootAlias expands a shell alias internally for nested lookup and
// returns completions rewritten to the user's alias form.
func (s *Session) expandRootAlias(ctx complete.Context, line, root string) []complete.Candidate {
	for _, a := range s.aliases.Aliases() {
		if a.Name != root {
			continue
		}
		expandedRoot := strings.Fields(a.Expansion)
		if len(expandedRoot) == 0 || s.registry.Lookup(expandedRoot[0]) == nil {
			return nil
		}
		expandedLine := a.Expansion + line[len(root):]
		cands := s.engine.Complete(ctx, expandedLine)
		for i := range cands {
			if strings.HasPrefix(cands[i].Text, a.Expansion) {
				cands[i].Text = root + strings.TrimPrefix(cands[i].Text, a.Expansion)
			}
		}
		return cands
	}
	return nil
}

// inferred attempts the Cobra __complete protocol for unknown executables.
func (s *Session) inferred(ctx complete.Context, line string, tokens []complete.Token) []complete.Candidate {
	partial := tokens[len(tokens)-1]
	var args []string
	for _, t := range tokens[1 : len(tokens)-1] {
		args = append(args, t.Text)
	}
	base := line[:partial.Start]
	cands := s.inferrer.Complete(ctx.CWD, tokens[0].Text, args, partial.Text)
	for i := range cands {
		cands[i].Text = base + ctx.Shell.QuoteArg(cands[i].Title)
		cands[i].Description = firstNonEmpty(cands[i].Description, "inferred completion")
	}
	return cands
}

// historyCandidates implements history mode: history matches with
// decreasing confidence from 75 to a floor of 60, merged with alias and
// spec results and sorted by confidence.
func (s *Session) historyCandidates(line, cwd string, cfg *config.Config) []complete.Candidate {
	matches := history.Search(s.hist.Entries(), line, s.aliasForms)
	cands := make([]complete.Candidate, 0, len(matches))
	for i, m := range matches {
		conf := 75 - i
		if conf < 60 {
			conf = 60
		}
		cands = append(cands, complete.Candidate{
			Text:        m.Entry.Command,
			Title:       m.Entry.Command,
			Description: historyAge(m.Entry),
			Source:      complete.SourceHistory,
			Confidence:  conf,
			Icon:        "history",
		})
	}
	for _, c := range s.specCandidates(line, cwd, cfg) {
		if c.Source != complete.SourceAlias && c.Source != complete.SourceSpec {
			continue
		}
		if c.Confidence == 0 {
			// Below history matches, ordered by spec priority.
			c.Confidence = min(44+c.Priority/4, 59)
		}
		cands = append(cands, c)
	}
	sortByConfidence(cands)
	return cands
}

// aliasForms returns alternate spellings for a history command so it can
// be found by alias or expansion.
func (s *Session) aliasForms(command string) []string {
	var forms []string
	for _, a := range s.aliases.Aliases() {
		if rest, ok := strings.CutPrefix(command, a.Name+" "); ok {
			forms = append(forms, a.Expansion+" "+rest)
		} else if command == a.Name {
			forms = append(forms, a.Expansion)
		}
		if rest, ok := strings.CutPrefix(command, a.Expansion+" "); ok {
			forms = append(forms, a.Name+" "+rest)
		} else if command == a.Expansion {
			forms = append(forms, a.Name)
		}
	}
	return forms
}

func historyAge(e history.Entry) string {
	if e.Time.IsZero() {
		return "history"
	}
	d := time.Since(e.Time)
	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return time.Duration(d.Round(time.Minute)).String() + " ago"
	case d < 24*time.Hour:
		return d.Round(time.Hour).String() + " ago"
	default:
		return e.Time.Format("2006-01-02")
	}
}

func sortByConfidence(cands []complete.Candidate) {
	for i := 1; i < len(cands); i++ {
		for j := i; j > 0 && cands[j].Confidence > cands[j-1].Confidence; j-- {
			cands[j], cands[j-1] = cands[j-1], cands[j]
		}
	}
}

// requestAI issues a debounced AI completion for the buffer.
func (s *Session) requestAI(line, cwd string) {
	snapshot := func() ai.Snapshot {
		s.mu.Lock()
		prevCommand := s.prevCommand
		prevExit := s.prevExit
		s.mu.Unlock()
		var recent []string
		for i, e := range s.hist.Entries() {
			if i >= 3 {
				break
			}
			recent = append([]string{e.Command}, recent...)
		}
		return s.gatherer.Gather(cwd, line, prevCommand, prevExit, recent)
	}()
	// Reuse a prefix-compatible suggestion before spending a provider
	// request, but only when its provider and gathered context still match.
	if text, ok := s.aiEngine.Cached(line, snapshot); ok {
		s.injectAI(line, text, ai.DefaultConfidence)
		return
	}
	s.aiEngine.Request(line, func() ai.Snapshot { return snapshot }, func(text string, confidence int) {
		s.injectAI(line, text, confidence)
	})
}

// injectAI inserts a validated AI candidate as the first suggestion when
// it is prefix-compatible and stronger than the current first result. It
// never disturbs a user-navigated selection, and an old response cannot
// overwrite a newer buffer.
func (s *Session) injectAI(requested, text string, confidence int) {
	s.mu.Lock()
	current := s.buf.String()
	if s.commandActive || s.navigated || current != requested || !strings.HasPrefix(text, current) {
		s.mu.Unlock()
		return
	}
	cand := complete.Candidate{
		Text:        text,
		Title:       text,
		Description: "AI suggestion",
		Source:      complete.SourceAI,
		Confidence:  confidence,
		Icon:        "ai",
	}
	if len(s.items) > 0 && s.items[0].Confidence >= confidence && s.items[0].Source != complete.SourceAI {
		// The local first result is stronger; keep AI below it.
		s.items = append([]complete.Candidate{s.items[0], cand}, s.items[1:]...)
		s.items = complete.Dedupe(s.items, current)
	} else {
		s.items = complete.Dedupe(append([]complete.Candidate{cand}, s.items...), current)
	}
	s.menuVisible = true
	s.selected = 0
	s.mu.Unlock()
	s.queueRender()
}

// maybePredictEmpty evaluates empty-prompt prediction after the
// configured debounce.
func (s *Session) maybePredictEmpty() {
	cfg := s.opts.Watcher.Current()
	if !cfg.AI.SuggestOnEmpty.Enabled {
		return
	}
	debounce := time.Duration(cfg.AI.SuggestOnEmpty.DebounceMS) * time.Millisecond
	if debounce == 0 {
		debounce = 800 * time.Millisecond
	}
	s.mu.Lock()
	gen := s.generation
	cwd := s.cwd
	prevCommand := s.prevCommand
	prevExit := s.prevExit
	if s.emptyTimer != nil {
		s.emptyTimer.Stop()
	}
	s.emptyTimer = time.AfterFunc(debounce, func() {
		s.launchWorker(func() {
			s.mu.Lock()
			stale := gen != s.generation || s.commandActive || !s.buf.Empty()
			s.mu.Unlock()
			if stale {
				return
			}
			// Local deterministic rules run first; only a rule with
			// confidence at least 70 short-circuits the AI fallback.
			if pred, ok := ai.PredictEmpty(cwd, prevCommand, prevExit, probe); ok && pred.Confidence >= 70 {
				s.apply(gen, []complete.Candidate{{
					Text:        pred.Command,
					Title:       pred.Command,
					Description: pred.Reason,
					Source:      complete.SourceAI,
					Confidence:  pred.Confidence,
					Icon:        "ai",
				}})
				return
			}
			if !cfg.AI.Enabled {
				return
			}
			minInterval := time.Duration(cfg.AI.SuggestOnEmpty.MinIntervalMS) * time.Millisecond
			if minInterval == 0 {
				minInterval = 5 * time.Second
			}
			s.aiEngine.RequestEmpty(minInterval, func() ai.Snapshot {
				return s.gatherer.Gather(cwd, "", prevCommand, prevExit, nil)
			}, func(command string) {
				s.mu.Lock()
				usable := gen == s.generation && !s.commandActive && s.buf.Empty()
				s.mu.Unlock()
				if !usable {
					return
				}
				s.apply(gen, []complete.Candidate{{
					Text:        command,
					Title:       command,
					Description: "AI prediction",
					Source:      complete.SourceAI,
					Confidence:  ai.DefaultConfidence,
					Icon:        "ai",
				}})
			})
		})
	})
	s.mu.Unlock()
}

func firstNonEmpty(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}
