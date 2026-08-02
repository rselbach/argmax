// Package engine is the suggestion orchestrator: it merges candidates from
// the spec registry, shell aliases, PATH executables, dynamic generators,
// tool aliases, Cobra inference, history, and optional AI, then de-duplicates
// and ranks them (PRD 9.6, 9.10, 9.11).
package engine

import (
	"context"
	"strings"
	"sync"

	"github.com/rselbach/argmax/internal/ai"
	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
	"github.com/rselbach/argmax/internal/history"
	"github.com/rselbach/argmax/internal/rank"
	"github.com/rselbach/argmax/internal/shell"
	"github.com/rselbach/argmax/internal/sources"
	"github.com/rselbach/argmax/internal/spec"
)

// Engine coordinates every candidate source for one session.
type Engine struct {
	reg *spec.Registry
	src *sources.Sources
	det *rank.Detector
	sh  shell.Shell

	mu       sync.Mutex
	cfg      *config.Config
	hist     *history.Provider
	store    *rank.Store
	aiEng    *ai.Engine
	cwd      string
	mode     core.Mode
	prevSk   string
	prevCmd  string
	prevExit int
	recent   []string // session commands, newest first, max 3 kept for AI
	closed   bool
}

// New builds an engine. Paths selects the history/database locations.
func New(cfg *config.Config, sh shell.Shell, cwd string, paths config.Paths) *Engine {
	e := &Engine{
		reg:  spec.Default(),
		src:  sources.New(cfg),
		det:  rank.NewDetector(),
		sh:   sh,
		cfg:  cfg,
		cwd:  cwd,
		mode: core.ModeSpec,
	}
	histPath := sh.HistoryPath(nil)
	e.hist = history.New(string(sh), histPath)
	if store, err := rank.Open(paths.DBFile); err == nil {
		e.store = store
	}
	e.aiEng = ai.New(&cfg.AI)
	return e
}

// SetConfig applies a live-reloaded configuration.
func (e *Engine) SetConfig(cfg *config.Config) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.cfg = cfg
	e.src.SetConfig(cfg)
	e.aiEng.UpdateConfig(&cfg.AI)
}

// SetCWD updates the tracked child-shell working directory (RUN-007).
// Relative updates are rejected.
func (e *Engine) SetCWD(cwd string) {
	if cwd == "" || cwd[0] != '/' {
		return
	}
	e.mu.Lock()
	e.cwd = cwd
	e.mu.Unlock()
}

// CWD returns the tracked working directory.
func (e *Engine) CWD() string {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.cwd
}

// CommandStarted records a command submission (preexec/buffer snapshot).
func (e *Engine) CommandStarted(cmd string) {
	cmd = strings.TrimSpace(cmd)
	if cmd == "" {
		return
	}
	e.mu.Lock()
	e.hist.AddSession(cmd)
	e.prevCmd = cmd
	if len(e.recent) == 0 || e.recent[0] != cmd {
		e.recent = append([]string{cmd}, e.recent...)
		if len(e.recent) > 3 {
			e.recent = e.recent[:3]
		}
	}
	e.mu.Unlock()
}

// CommandFinished records the exit status and feeds the learning store
// (RANK-002): skeleton transitions from the previous command.
func (e *Engine) CommandFinished(exitCode int) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.prevCmd == "" {
		e.prevExit = exitCode
		return
	}
	sk := e.reg.Skeleton(e.prevCmd)
	if e.store != nil {
		e.store.Record(e.cwd, sk, e.prevSk, exitCode)
	}
	e.prevSk = sk
	e.prevExit = exitCode
	e.prevCmd = ""
}

// PrevCommand returns the last submitted command and its exit code.
func (e *Engine) PrevCommand() (string, int) {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.prevCmd, e.prevExit
}

// Mode returns the active suggestion mode.
func (e *Engine) Mode() core.Mode {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.mode
}

// SetMode switches between spec and history mode.
func (e *Engine) SetMode(m core.Mode) {
	e.mu.Lock()
	e.mode = m
	e.mu.Unlock()
}

// Close releases resources.
func (e *Engine) Close() {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.closed {
		return
	}
	e.closed = true
	if e.store != nil {
		_ = e.store.Close()
	}
}

// ShellAliases returns the discovered shell aliases.
func (e *Engine) ShellAliases() []sources.Alias {
	return e.src.ShellAliases(string(e.sh))
}

func (e *Engine) aliasMap() map[string]string {
	aliases := e.ShellAliases()
	m := make(map[string]string, len(aliases))
	for _, a := range aliases {
		m[a.Name] = a.Expansion
	}
	return m
}

// ExpandAlias implements SRC-005: when the buffer is exactly one shell alias
// and expand-alias is enabled, return its expansion.
func (e *Engine) ExpandAlias(buffer string) (string, bool) {
	e.mu.Lock()
	cfg := e.cfg
	e.mu.Unlock()
	if !cfg.Core.ExpandAlias {
		return "", false
	}
	if strings.TrimSpace(buffer) != buffer || strings.ContainsAny(buffer, " \t") {
		return "", false
	}
	for _, a := range e.ShellAliases() {
		if a.Name == buffer {
			return a.Expansion, true
		}
	}
	return "", false
}

// HistoryRecent returns the newest unique history commands for an empty
// buffer (IN-007).
func (e *Engine) HistoryRecent(limit int) []core.Suggestion {
	matches := e.hist.Search("", e.aliasMap(), limit)
	out := make([]core.Suggestion, 0, len(matches))
	for i, m := range matches {
		out = append(out, core.Suggestion{
			Text:        m.Command,
			Description: "history",
			Icon:        "history",
			Source:      core.SourceHistory,
			Confidence:  historyConfidence(i, len(matches)),
			Priority:    -1,
		})
	}
	return out
}

// Suggest computes the ranked candidate set for buffer in the active mode.
// ctx bounds dynamic work; results after ctx cancellation are static-only.
func (e *Engine) Suggest(ctx context.Context, buffer string) []core.Suggestion {
	e.mu.Lock()
	cfg := e.cfg
	mode := e.mode
	e.mu.Unlock()

	var out []core.Suggestion
	if mode == core.ModeHistory {
		out = e.historySuggest(buffer, cfg)
	} else {
		out = e.specSuggest(ctx, buffer, cfg)
	}
	// SRC-009/010: de-duplicate by final text, drop exact query copies, cap.
	out = dedupe(out, buffer)
	if len(out) > cfg.UI.MaxSuggestions {
		out = out[:cfg.UI.MaxSuggestions]
	}
	return out
}

// specSuggest merges aliases, specs, PATH executables, generators, tool
// aliases, and Cobra inference for spec mode (SRC-001).
func (e *Engine) specSuggest(ctx context.Context, buffer string, cfg *config.Config) []core.Suggestion {
	tokens := spec.Tokenize(buffer)
	cwd := e.CWD()

	// Top level: completing the command word itself.
	if len(tokens) <= 1 {
		partial := ""
		if len(tokens) == 1 {
			partial = tokens[0]
		}
		var cands []core.Suggestion
		cands = append(cands, e.aliasCandidates(partial)...)
		cands = append(cands, e.reg.TopLevel(partial)...)
		cands = append(cands, e.src.Executables(partial)...)
		return e.rank(cands, partial, cwd, cfg)
	}

	// Resolve the root through a shell alias for nested lookup (SRC-006).
	line := buffer
	aliasPrefix, aliasExp := "", ""
	rootToken := firstToken(buffer)
	for _, a := range e.ShellAliases() {
		if a.Name == rootToken {
			aliasPrefix, aliasExp = a.Name, a.Expansion
			line = a.Expansion + buffer[len(rootToken):]
			break
		}
	}

	res := e.reg.Resolve(line)

	// Git/Cargo alias traversal (SRC-008): rewrite `git <alias>` into its
	// expansion for nested completion, keeping the alias spelling in results.
	// Aliases whose expansion begins with '!' are not traversed.
	toolPrefix, toolExp := "", ""
	if res.Root != nil && len(res.NodePath) >= 1 {
		if ta, ok := e.toolAliasAt(res, cwd); ok && !ta.Shell {
			if exp := strings.Fields(ta.Expansion); len(exp) > 0 {
				rewritten := res.Root.Name + " " + strings.Join(exp, " ") + remainderAfter(line, res.Root.Name, ta.Name)
				toolExp = res.Root.Name + " " + strings.Join(exp, " ")
				toolPrefix = res.Root.Name + " " + ta.Name
				res = e.reg.Resolve(rewritten)
			}
		}
	}

	var cands []core.Suggestion
	if res.Root != nil && !res.Dead {
		cands = append(cands, e.reg.StaticCandidates(res)...)
		// Tool aliases at the subcommand position.
		if len(res.NodePath) == 1 && !res.Dash && (res.Root.Name == "git" || res.Root.Name == "cargo") {
			cands = append(cands, e.toolAliasCandidates(res.Root.Name, res.Partial, cwd)...)
		}
		if id, args, ok := e.reg.GeneratorRequest(res); ok && !res.MaxedOut {
			gen := e.src.Generate(ctx, sources.GenRequest{
				ID:      id,
				RootCmd: res.Root.Name,
				Args:    args,
				Partial: res.Partial,
				CWD:     cwd,
			})
			for _, g := range gen {
				g.Text = res.LinePrefix + spec.QuoteIfNeeded(g.Text)
				cands = append(cands, g)
			}
		}
	} else if res.Root == nil {
		// Unknown command: try Cobra inference, then plain file completion.
		root := spec.Tokenize(line)[0]
		args := []string{}
		if tk := spec.Tokenize(line); len(tk) > 1 {
			args = tk[1 : len(tk)-1]
		}
		cobra := e.src.CobraComplete(ctx, root, args, res.Partial)
		for _, c := range cobra {
			c.Text = res.LinePrefix + spec.QuoteIfNeeded(c.Text)
			cands = append(cands, c)
		}
		if len(cands) == 0 {
			for _, f := range e.src.CompleteFiles(sources.FileRequest{
				Partial:    res.Partial,
				CWD:        cwd,
				Mode:       sources.FileAny,
				ShowHidden: cfg.UI.HiddenFiles,
			}) {
				f.Text = res.LinePrefix + spec.QuoteIfNeeded(f.Text)
				cands = append(cands, f)
			}
		}
	}

	// Map alias-form expansions back to the user's spelling (SRC-006).
	if aliasExp != "" {
		for i := range cands {
			if rest, ok := strings.CutPrefix(cands[i].Text, aliasExp); ok {
				cands[i].Text = aliasPrefix + rest
			}
		}
	}
	if toolPrefix != "" {
		// Completions were computed for the expanded tool alias; rewrite the
		// expanded prefix back to `git <alias>` form.
		for i := range cands {
			if rest, ok := strings.CutPrefix(cands[i].Text, toolExp); ok {
				cands[i].Text = toolPrefix + rest
			}
		}
	}

	return e.rank(cands, lastPartial(buffer), cwd, cfg)
}

// historySuggest implements HIST-011: history-first results merged with
// alias/spec matches, sorted by confidence.
func (e *Engine) historySuggest(buffer string, cfg *config.Config) []core.Suggestion {
	query := strings.TrimSpace(buffer)
	matches := e.hist.Search(query, e.aliasMap(), cfg.UI.MaxSuggestions)
	out := make([]core.Suggestion, 0, len(matches)+4)
	for i, m := range matches {
		out = append(out, core.Suggestion{
			Text:        m.Command,
			Description: "history",
			Icon:        "history",
			Source:      core.SourceHistory,
			Confidence:  historyConfidence(i, len(matches)),
			Priority:    -1,
		})
	}
	// Retain relevant alias and spec results (HIST-011).
	if query != "" {
		for _, c := range e.aliasCandidates(query) {
			c.Confidence = 65
			out = append(out, c)
		}
		for _, c := range e.reg.TopLevel(query) {
			c.Confidence = 55
			out = append(out, c)
		}
	}
	// Sort by confidence desc, stable.
	for i := 1; i < len(out); i++ {
		for j := i; j > 0 && out[j].Confidence > out[j-1].Confidence; j-- {
			out[j], out[j-1] = out[j-1], out[j]
		}
	}
	if len(out) > cfg.UI.MaxSuggestions {
		out = out[:cfg.UI.MaxSuggestions]
	}
	return out
}

// historyConfidence decreases from 75 to a floor of 60 across the list
// (HIST-011).
func historyConfidence(i, total int) int {
	if total <= 1 {
		return 75
	}
	c := 75 - (i*15)/(total-1)
	if c < 60 {
		return 60
	}
	return c
}

// aliasCandidates returns shell-alias candidates at the top level (SRC-004).
func (e *Engine) aliasCandidates(partial string) []core.Suggestion {
	if partial == "" {
		return nil
	}
	var out []core.Suggestion
	for _, a := range e.ShellAliases() {
		if strings.HasPrefix(strings.ToLower(a.Name), strings.ToLower(partial)) && a.Name != partial {
			out = append(out, core.Suggestion{
				Text:        a.Name,
				Description: "alias for '" + a.Expansion + "'",
				Icon:        "alias",
				Source:      core.SourceAlias,
				Confidence:  90,
				Priority:    85,
			})
		}
	}
	return out
}

// toolAliasCandidates returns git/cargo alias candidates at the subcommand
// position (SRC-008).
func (e *Engine) toolAliasCandidates(root, partial string, cwd string) []core.Suggestion {
	var out []core.Suggestion
	for _, ta := range e.toolAliasesFor(root, cwd) {
		if partial != "" && !strings.HasPrefix(strings.ToLower(ta.Name), strings.ToLower(partial)) {
			continue
		}
		if ta.Name == partial {
			continue
		}
		out = append(out, core.Suggestion{
			Text:        root + " " + ta.Name,
			Description: root + " " + ta.Expansion,
			Icon:        root,
			Source:      core.SourceToolAlias,
			Confidence:  90,
			Priority:    ta.Scope,
		})
	}
	return out
}

func (e *Engine) toolAliasesFor(root, cwd string) []sources.ToolAlias {
	if root == "git" {
		return e.src.GitAliases(cwd)
	}
	return e.src.CargoAliases(cwd)
}

// toolAliasAt finds a tool alias matching the first arg after the root.
func (e *Engine) toolAliasAt(res spec.Result, cwd string) (sources.ToolAlias, bool) {
	tokens := spec.Tokenize(res.Line)
	if len(tokens) < 2 {
		return sources.ToolAlias{}, false
	}
	name := tokens[1]
	for _, ta := range e.toolAliasesFor(res.Root.Name, cwd) {
		if ta.Name == name {
			return ta, true
		}
	}
	return sources.ToolAlias{}, false
}

// rank scores and sorts candidates via the rank package.
func (e *Engine) rank(cands []core.Suggestion, partial, cwd string, cfg *config.Config) []core.Suggestion {
	if len(cands) == 0 {
		return nil
	}
	e.mu.Lock()
	store := e.store
	prevSk := e.prevSk
	e.mu.Unlock()

	rc := make([]rank.Candidate, len(cands))
	for i, c := range cands {
		rc[i] = rank.Candidate{
			Suggestion:   c,
			Skeleton:     e.reg.Skeleton(c.Text),
			MatchQuality: matchQuality(partial, lastTokenOf(c.Text)),
		}
	}
	env := rank.Env{
		CWD:          cwd,
		WS:           e.det.Detect(cwd),
		Store:        store,
		PrevSkeleton: prevSk,
	}
	scored := rank.Score(rc, env)
	out := make([]core.Suggestion, len(scored))
	for i, s := range scored {
		out[i] = s.Suggestion
	}
	return out
}

// InjectAI merges an AI completion into an existing result set (AI-011,
// SRC-011): it replaces a lower-confidence duplicate, or is inserted first
// only when stronger than the current top result. Returns the (possibly
// unchanged) set.
func InjectAI(results []core.Suggestion, text string) []core.Suggestion {
	if text == "" {
		return results
	}
	aiCand := core.Suggestion{
		Text:        text,
		Description: "AI suggestion",
		Icon:        "ai",
		Source:      core.SourceAI,
		Confidence:  85,
		Priority:    -1,
	}
	for i, r := range results {
		if r.Text == text {
			if r.Confidence < aiCand.Confidence {
				results[i] = aiCand
			}
			return results
		}
	}
	if len(results) == 0 || effectiveStrength(results[0]) < effectiveStrength(aiCand) {
		return append([]core.Suggestion{aiCand}, results...)
	}
	return results
}

func effectiveStrength(s core.Suggestion) int {
	if s.Priority >= 0 && s.Priority > s.Confidence {
		return s.Priority
	}
	return s.Confidence
}

// AISuggest requests an AI completion for buffer (AI-005..011 gates are
// enforced here and in the ai package).
func (e *Engine) AISuggest(ctx context.Context, buffer string) (string, bool) {
	e.mu.Lock()
	cfg := e.cfg
	prevCmd, prevExit := e.prevCmd, e.prevExit
	recent := append([]string(nil), e.recent...)
	cwd := e.cwd
	e.mu.Unlock()
	if !cfg.AI.Enabled {
		return "", false
	}
	return e.aiEng.Suggest(ctx, ai.Request{
		Buffer:      buffer,
		CWD:         cwd,
		PrevCommand: prevCmd,
		PrevExit:    prevExit,
		Recent:      recent,
	})
}

// AIEnabled reports whether AI completion is active.
func (e *Engine) AIEnabled() bool { return e.aiEng.Enabled() }

// EmptyPrompt returns opt-in empty-prompt predictions (EMPTY-001..003).
func (e *Engine) EmptyPrompt(ctx context.Context) []core.Suggestion {
	e.mu.Lock()
	cfg := e.cfg
	cwd := e.cwd
	prevCmd, prevExit := e.prevCmd, e.prevExit
	e.mu.Unlock()
	if !cfg.AI.Enabled || !cfg.AI.SuggestOnEmpty.Enabled {
		return nil
	}
	return ai.EmptyPromptSuggestions(ctx, cwd, prevCmd, prevExit)
}

// Skeleton exposes the registry skeleton for the session's learning hooks.
func (e *Engine) Skeleton(line string) string { return e.reg.Skeleton(line) }

// dedupe removes duplicate final texts and exact query copies (SRC-009),
// keeping the first (highest-ranked) occurrence.
func dedupe(cands []core.Suggestion, query string) []core.Suggestion {
	seen := make(map[string]bool, len(cands))
	out := make([]core.Suggestion, 0, len(cands))
	for _, c := range cands {
		if c.Text == "" || seen[c.Text] {
			continue
		}
		if c.Text == query && c.Source != core.SourceAlias && c.Source != core.SourceToolAlias {
			continue
		}
		seen[c.Text] = true
		out = append(out, c)
	}
	return out
}

// matchQuality implements PRD 10.2 against the completed final token.
func matchQuality(partial, value string) float64 {
	if partial == "" {
		return 100
	}
	if value == partial || strings.HasPrefix(value, partial) {
		return 100
	}
	lp, lv := strings.ToLower(partial), strings.ToLower(value)
	if strings.HasPrefix(lv, lp) {
		return 80
	}
	if strings.Contains(lv, lp) {
		return 50
	}
	if isSubsequence(lp, lv) {
		return 30
	}
	return 0
}

func isSubsequence(needle, haystack string) bool {
	i := 0
	for j := 0; j < len(haystack) && i < len(needle); j++ {
		if needle[i] == haystack[j] {
			i++
		}
	}
	return i == len(needle)
}

func firstToken(line string) string {
	tk := spec.Tokenize(line)
	if len(tk) == 0 {
		return ""
	}
	return tk[0]
}

func lastPartial(line string) string {
	tk := spec.Tokenize(line)
	if len(tk) == 0 {
		return ""
	}
	return tk[len(tk)-1]
}

func lastTokenOf(text string) string {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return text
	}
	return fields[len(fields)-1]
}

// remainderAfter returns line with the leading `root name` removed, keeping
// the remainder verbatim (or "" when nothing follows).
func remainderAfter(line, root, name string) string {
	idx := strings.Index(line, name)
	if idx < 0 {
		return ""
	}
	rest := line[idx+len(name):]
	return rest
}
