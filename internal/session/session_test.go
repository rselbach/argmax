package session

import (
	"bytes"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/completion"
	"github.com/rselbach/argmax/internal/coordinator"
	"github.com/rselbach/argmax/internal/shellevents"
)

const testProvider = "test"

func newTestReducer(t *testing.T) (*Reducer, *shellevents.Decoder) {
	t.Helper()
	epoch := shellevents.InitialStreamEpoch()
	reducer, err := New(epoch, []byte("\x12"), []byte("\x10"), []string{testProvider}, 10, []byte("/tmp"))
	if err != nil {
		t.Fatal(err)
	}
	return reducer, shellevents.NewDecoder(epoch)
}

func applyWire(t *testing.T, reducer *Reducer, decoder *shellevents.Decoder, wire []byte) []EffectBatch {
	t.Helper()
	var batches []EffectBatch
	decoder.Push(wire, func(frame shellevents.DecodedFrame) {
		_, effects := reducer.ApplyShellFrame(frame)
		batches = append(batches, effects)
	})
	return batches
}

func readyReducer(t *testing.T) (*Reducer, *shellevents.Decoder) {
	t.Helper()
	reducer, decoder := newTestReducer(t)
	applyWire(t, reducer, decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	return reducer, decoder
}

func findSyncNonce(batch EffectBatch) (uint64, bool) {
	for _, effect := range batch.Effects() {
		if nonce, ok := effect.BufferSyncNonce(); ok {
			return nonce.Value(), true
		}
	}
	return 0, false
}

func synchronize(t *testing.T, reducer *Reducer, decoder *shellevents.Decoder, typed []byte, line string, cursor int) uint64 {
	t.Helper()
	reduction := reducer.RouteInput(typed)
	nonce, ok := findSyncNonce(reduction.Effects())
	if !ok {
		t.Fatalf("missing synchronization effect: %s", reduction.Effects())
	}
	applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:%d:%s\x00", nonce, cursor, line)))
	query, ok := reducer.ActiveQuery()
	if !ok {
		t.Fatal("missing active query")
	}
	return query.Generation()
}

func testSuggestion(t *testing.T, line, replacement string, source completion.SuggestionSource, insertion completion.InsertionBehavior) completion.Suggestion {
	t.Helper()
	edit, err := completion.NewTextEdit(0, len(line), replacement)
	if err != nil {
		t.Fatal(err)
	}
	candidate, err := completion.NewSuggestion(edit, replacement, "description", "command", source, insertion, replacement)
	if err != nil {
		t.Fatal(err)
	}
	return candidate
}

func present(t *testing.T, reducer *Reducer, generation uint64, candidate completion.Suggestion) {
	t.Helper()
	if _, ok := reducer.AcceptProviderBatch(completion.NewSuccessBatch(testProvider, generation, []completion.Suggestion{candidate})).Acceptance(); !ok {
		t.Fatal("provider batch rejected")
	}
	if got := reducer.ApplyRankedCandidates(generation, []completion.Suggestion{candidate}).Outcome().Kind(); got != coordinator.PresentationApplied {
		t.Fatalf("presentation = %v", got)
	}
}

func forwarded(batch EffectBatch) []byte {
	var result []byte
	for _, effect := range batch.Effects() {
		if input, ok := effect.ForwardInput(); ok {
			result = append(result, input...)
		}
	}
	return result
}

func effectIndex(batch EffectBatch, kind EffectKind) int {
	for index, effect := range batch.Effects() {
		if effect.Kind() == kind {
			return index
		}
	}
	return -1
}

func queryEffect(batches []EffectBatch) (coordinator.QueryWork, bool, bool) {
	for _, batch := range batches {
		for _, effect := range batch.Effects() {
			_, alias, work, ok := effect.Query()
			if ok {
				return work, alias, true
			}
		}
	}
	return coordinator.QueryWork{}, false, false
}

func requestHistoryPreview(t *testing.T, reducer *Reducer, original, preview string) uint64 {
	t.Helper()
	toggle := reducer.RouteInput([]byte("\x12"))
	var generation uint64
	found := false
	for _, effect := range toggle.Effects().Effects() {
		mode, _, work, ok := effect.Query()
		if ok && mode == ModeHistory {
			generation, found = work.Query().Generation(), true
		}
	}
	if !found {
		t.Fatal("missing history query")
	}
	present(t, reducer, generation, testSuggestion(t, original, preview, completion.SourceHistory, completion.InsertionExact))
	result := reducer.RouteInput([]byte("\x1b[A"))
	nonce, ok := findSyncNonce(result.Effects())
	if !ok {
		t.Fatalf("missing preview sync: %s", result.Effects())
	}
	return nonce
}

func hasEffect(batch EffectBatch, kind EffectKind) bool { return effectIndex(batch, kind) >= 0 }

func TestNewWithModeRejectsInvalidMode(t *testing.T) {
	t.Parallel()
	tests := map[string]Mode{
		"zero":    0,
		"unknown": ModeHistory + 1,
	}
	for name, mode := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := NewWithMode(
				shellevents.InitialStreamEpoch(), []byte("\x12"), []byte("\x10"),
				[]string{testProvider}, 10, []byte("/tmp"), mode,
			)
			var buildErr *BuildError
			if !errors.As(err, &buildErr) || buildErr.Kind() != BuildInvalidMode {
				t.Fatalf("error = %#v", err)
			}
		})
	}
}

func TestTabReplacementPrecedesSyncAndSameBatchEnter(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("git che"), "git che", 7)
	present(t, reducer, generation, testSuggestion(t, "git che", "git checkout", completion.SourceSpec, completion.InsertionAppendSpace))

	reduction := reducer.RouteInput([]byte("\t\r"))
	effects := reduction.Effects()
	if got := forwarded(effects); !bytes.Equal(got, []byte("\r")) {
		t.Fatalf("forwarded = %q", got)
	}
	replace, sync, enter := effectIndex(effects, EffectReplaceBuffer), effectIndex(effects, EffectRequestBufferSync), effectIndex(effects, EffectForwardInput)
	if replace < 0 || replace >= sync || sync >= enter {
		t.Fatalf("effect order = %s", effects)
	}
	replacement, _ := effects.Effects()[replace].Replacement()
	if replacement.Text() != "git checkout " || replacement.Cursor() != len("git checkout ") {
		t.Fatalf("replacement = %s", replacement)
	}
}

func TestHistoryPreviewAndEnterSameBatchPairReplacementWithSync(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	toggle := reducer.RouteInput([]byte("\x12"))
	var generation uint64
	for _, effect := range toggle.Effects().Effects() {
		mode, _, work, ok := effect.Query()
		if ok && mode == ModeHistory {
			generation = work.Query().Generation()
		}
	}
	present(t, reducer, generation, testSuggestion(t, "git", "git status", completion.SourceHistory, completion.InsertionExact))

	reduction := reducer.RouteInput([]byte("\x1b[A\r"))
	effects := reduction.Effects()
	replace, sync, enter := effectIndex(effects, EffectReplaceBuffer), effectIndex(effects, EffectRequestBufferSync), effectIndex(effects, EffectForwardInput)
	if replace < 0 || replace >= sync || sync >= enter {
		t.Fatalf("effect order = %s", effects)
	}
}

func TestToggleWithPreviewSyncOutstandingKeepsHistoryForRetry(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	nonce := requestHistoryPreview(t, reducer, "git", "git status")

	blocked := reducer.RouteInput([]byte("\x12"))
	if hasEffect(blocked.Effects(), EffectModeChanged) || len(forwarded(blocked.Effects())) != 0 || reducer.Mode() != ModeHistory {
		t.Fatalf("toggle changed pending preview: %s, mode %s", blocked.Effects(), reducer.Mode())
	}
	applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", nonce)))
	retry := reducer.RouteInput([]byte("\x12"))
	if reducer.Mode() != ModeSpec || !hasEffect(retry.Effects(), EffectReplaceBuffer) {
		t.Fatalf("retry = %s, mode %s", retry.Effects(), reducer.Mode())
	}
}

func TestGhostRefusesInvisibleMultilineSuffix(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("printf"), "printf", 6)
	present(t, reducer, generation, testSuggestion(t, "printf", "printf 'one\ntwo'", completion.SourceHistory, completion.InsertionExact))

	reduction := reducer.RouteInput([]byte("\x1b[C"))
	if hasEffect(reduction.Effects(), EffectReplaceBuffer) || !bytes.Equal(forwarded(reduction.Effects()), []byte("\x1b[C")) {
		t.Fatalf("reduction = %s", reduction.Effects())
	}
}

func TestFinishInputDrainsPasteAndPartialSequence(t *testing.T) {
	t.Run("truncated paste reenables actions", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		reducer.RouteInput([]byte("\x1b[200~pasted"))
		drained := reducer.FinishInput()
		nonce, ok := findSyncNonce(drained.Effects())
		if !ok {
			t.Fatalf("missing drain sync: %s", drained.Effects())
		}
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:3:git\x00", nonce)))
		if toggle := reducer.RouteInput([]byte("\x12")); !hasEffect(toggle.Effects(), EffectModeChanged) {
			t.Fatalf("toggle = %s", toggle.Effects())
		}
	})
	t.Run("partial sequence forwarded intact", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		if retained := reducer.RouteInput([]byte("\x1b[")); hasEffect(retained.Effects(), EffectForwardInput) {
			t.Fatalf("retained = %s", retained.Effects())
		}
		if got := forwarded(reducer.FinishInput().Effects()); !bytes.Equal(got, []byte("\x1b[")) {
			t.Fatalf("forwarded = %q", got)
		}
	})
}

func TestEnterExecutesAuthoritativeBufferNotSelection(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("git che"), "git che", 7)
	present(t, reducer, generation, testSuggestion(t, "git che", "git cherry-pick", completion.SourceSpec, completion.InsertionExact))

	reduction := reducer.RouteInput([]byte("\r"))
	if !bytes.Equal(forwarded(reduction.Effects()), []byte("\r")) || hasEffect(reduction.Effects(), EffectReplaceBuffer) {
		t.Fatalf("reduction = %s", reduction.Effects())
	}
	if effectIndex(reduction.Effects(), EffectClearOverlay) > effectIndex(reduction.Effects(), EffectForwardInput) {
		t.Fatalf("overlay not cleared before Enter: %s", reduction.Effects())
	}
}

func TestBracketedPasteIsExactWithoutAcceptanceOrAlias(t *testing.T) {
	reducer, decoder := readyReducer(t)
	paste := []byte("\x1b[200~gs \nwhoami && ll\x1b[201~")
	reduction := reducer.RouteInput(paste)
	if !bytes.Equal(forwarded(reduction.Effects()), paste) || hasEffect(reduction.Effects(), EffectReplaceBuffer) || hasEffect(reduction.Effects(), EffectStartQuery) {
		t.Fatalf("reduction = %s", reduction.Effects())
	}
	nonce, ok := findSyncNonce(reduction.Effects())
	if !ok {
		t.Fatal("missing post-paste synchronization")
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:3:gs \x00", nonce)))
	_, alias, ok := queryEffect(batches)
	if !ok || alias {
		t.Fatalf("query alias = %t, found %t", alias, ok)
	}
}

func TestUnicodeCharacterCursorUsesByteOffset(t *testing.T) {
	reducer, decoder := readyReducer(t)
	reduction := reducer.RouteInput([]byte("x"))
	nonce, _ := findSyncNonce(reduction.Effects())
	applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:c:%d:4:echo 世界\x00", nonce)))
	query, ok := reducer.ActiveQuery()
	if !ok || query.Line() != "echo 世界" || query.Cursor() != 4 || query.Prefix() != "echo" {
		t.Fatalf("query = %s, ok %t", query, ok)
	}
}

func TestLateResultsCannotRestoreSelection(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("g"), "g", 1)
	candidate := testSuggestion(t, "g", "git", completion.SourceSpec, completion.InsertionExact)
	reducer.RouteInput([]byte("i"))
	if _, ok := reducer.AcceptProviderBatch(completion.NewSuccessBatch(testProvider, generation, []completion.Suggestion{candidate})).Rejection(); !ok {
		t.Fatal("late provider batch accepted")
	}
	if got := reducer.ApplyRankedCandidates(generation, []completion.Suggestion{candidate}).Outcome().Kind(); got != coordinator.PresentationRejected {
		t.Fatalf("late presentation = %v", got)
	}
	selectionState := reducer.Selection()
	if selectionState.IsVisible() {
		t.Fatal("late result restored selection")
	}
}

func TestHistoryPreviewAuthorityTransitions(t *testing.T) {
	t.Run("mismatch uses accepted shell buffer", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		nonce := requestHistoryPreview(t, reducer, "git", "git status")
		batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git branch\x00", nonce)))
		work, _, ok := queryEffect(batches)
		if !ok || work.Query().Line() != "git branch" || work.Query().Cursor() != 10 || reducer.historyPreviewActive || reducer.historyOrigin != nil {
			t.Fatalf("work = %s, preview %t, origin %v", work, reducer.historyPreviewActive, reducer.historyOrigin)
		}
	})
	t.Run("prompt clears preview authority", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		nonce := requestHistoryPreview(t, reducer, "git", "git status")
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", nonce)))
		batches := applyWire(t, reducer, decoder, []byte("prompt-ready\x00"))
		work, _, ok := queryEffect(batches)
		if !ok || work.Query().Line() != "" || work.Query().Cursor() != 0 || reducer.historyPreviewActive || reducer.historyOrigin != nil {
			t.Fatalf("work = %s, preview %t", work, reducer.historyPreviewActive)
		}
	})
	t.Run("rejected frame abandons restore", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		nonce := requestHistoryPreview(t, reducer, "git", "git status")
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", nonce)))
		applyWire(t, reducer, decoder, []byte("not-an-event\x00"))
		reducer.RouteInput([]byte("\x1b"))
		escape := reducer.FlushInput()
		if !bytes.Equal(forwarded(escape.Effects()), []byte("\x1b")) || hasEffect(escape.Effects(), EffectReplaceBuffer) {
			t.Fatalf("escape = %s", escape.Effects())
		}
	})
}

func TestCursorActionsAbandonHistoryOriginBeforeResync(t *testing.T) {
	cases := map[string]struct {
		input  []byte
		cursor int
	}{
		"left": {[]byte("\x1b[D"), 9}, "home": {[]byte("\x1b[H"), 0},
		"end": {[]byte("\x1b[F"), 10}, "ctrl-a": {[]byte("\x01"), 0}, "ctrl-e": {[]byte("\x05"), 10},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			reducer, decoder := readyReducer(t)
			synchronize(t, reducer, decoder, []byte("git"), "git", 3)
			previewNonce := requestHistoryPreview(t, reducer, "git", "git status")
			applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", previewNonce)))
			action := reducer.RouteInput(tc.input)
			if !bytes.Equal(forwarded(action.Effects()), tc.input) || reducer.historyPreviewActive || reducer.historyOrigin != nil {
				t.Fatalf("action = %s", action.Effects())
			}
			nonce, ok := findSyncNonce(action.Effects())
			if !ok {
				t.Fatal("missing action sync")
			}
			applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:%d:git status\x00", nonce, tc.cursor)))
			query, ok := reducer.ActiveQuery()
			if !ok || query.Cursor() != tc.cursor || query.Line() != "git status" {
				t.Fatalf("query = %s", query)
			}
		})
	}
}

func TestTypedAliasSpaceAndStaleExpansion(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("gs"), "gs", 2)
	space := reducer.RouteInput([]byte(" "))
	nonce, ok := findSyncNonce(space.Effects())
	if !ok || !bytes.Equal(forwarded(space.Effects()), []byte(" ")) {
		t.Fatalf("space = %s", space.Effects())
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:3:gs \x00", nonce)))
	work, alias, ok := queryEffect(batches)
	if !ok || !alias || work.Query().Line() != "gs " {
		t.Fatalf("work = %s, alias %t", work, alias)
	}
	edit, _ := completion.NewTextEdit(0, 2, "git status")
	if effects := reducer.ApplyAliasExpansion(work.Query().Generation()+1, edit); !effects.Empty() {
		t.Fatalf("stale expansion = %s", effects)
	}
	effects := reducer.ApplyAliasExpansion(work.Query().Generation(), edit)
	if kinds := []EffectKind{EffectClearOverlay, EffectReplaceBuffer, EffectRequestBufferSync}; len(effects.Effects()) != len(kinds) {
		t.Fatalf("expansion = %s", effects)
	} else {
		for index, want := range kinds {
			if effects.Effects()[index].Kind() != want {
				t.Fatalf("expansion = %s", effects)
			}
		}
	}
	replacement, _ := effects.Effects()[1].Replacement()
	if replacement.Text() != "git status " || replacement.Cursor() != 11 || !reducer.ReplacementPending() {
		t.Fatalf("replacement = %s", replacement)
	}
}

func TestShellOutputResynchronizesActiveBuffer(t *testing.T) {
	reducer, decoder := readyReducer(t)
	first := synchronize(t, reducer, decoder, []byte("ssh-"), "ssh-", 4)
	effects := reducer.ObserveShellOutput()
	nonce, ok := findSyncNonce(effects)
	if !ok {
		t.Fatalf("output effects = %s", effects)
	}
	if _, active := reducer.ActiveQuery(); active {
		t.Fatal("query remained active")
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:4:ssh-\x00", nonce)))
	work, _, ok := queryEffect(batches)
	if !ok || work.Query().Line() != "ssh-" || work.Query().Generation() <= first {
		t.Fatalf("work = %s", work)
	}
}

func TestCWDUpdateAtomicAndDeferred(t *testing.T) {
	t.Run("bounded atomic restart", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		old := synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		effects, err := reducer.UpdateCWD([]byte("/var/tmp"))
		if err != nil {
			t.Fatal(err)
		}
		work, _, ok := queryEffect([]EffectBatch{effects})
		if !ok || !bytes.Equal(work.Query().CWD(), []byte("/var/tmp")) || work.Query().Generation() == old {
			t.Fatalf("work = %s", work)
		}
		query, _ := reducer.ActiveQuery()
		if _, err := reducer.UpdateCWD([]byte("relative/path")); err == nil || !bytes.Equal(reducer.CWD(), []byte("/var/tmp")) {
			t.Fatalf("relative update changed state: %v", err)
		}
		after, _ := reducer.ActiveQuery()
		if after.Generation() != query.Generation() {
			t.Fatal("invalid update cancelled query")
		}
		oversized := append([]byte{'/'}, bytes.Repeat([]byte{'a'}, coordinator.MaxQueryCWDBytes)...)
		if _, err := reducer.UpdateCWD(oversized); err == nil {
			t.Fatal("oversized cwd accepted")
		}
	})
	t.Run("paste defers restart", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		reducer.RouteInput([]byte("\x1b[200~"))
		update, err := reducer.UpdateCWD([]byte("/var/tmp"))
		if err != nil || !reducer.QueryRestartDeferred() || hasEffect(update, EffectStartQuery) {
			t.Fatalf("update = %s, deferred %t, err %v", update, reducer.QueryRestartDeferred(), err)
		}
		end := reducer.RouteInput([]byte("\x1b[201~"))
		work, _, ok := queryEffect([]EffectBatch{end.Effects()})
		if !ok || work.Query().Line() != "git" || !bytes.Equal(work.Query().CWD(), []byte("/var/tmp")) || reducer.QueryRestartDeferred() {
			t.Fatalf("work = %s, deferred %t", work, reducer.QueryRestartDeferred())
		}
	})
}

func TestSensitiveValuesAreRedacted(t *testing.T) {
	replacement, err := NewBufferReplacement("secret-token", 12)
	if err != nil {
		t.Fatal(err)
	}
	batch := EffectBatch{effects: []Effect{
		{kind: EffectForwardInput, input: []byte("hunter2")},
		{kind: EffectReplaceBuffer, replacement: replacement},
	}}
	debug := fmt.Sprintf("%#v", batch)
	if strings.Contains(debug, "hunter2") || strings.Contains(debug, "secret-token") || !strings.Contains(debug, "byte_count") {
		t.Fatalf("debug leaked content: %s", debug)
	}

	reducer, _ := readyReducer(t)
	if debug = fmt.Sprintf("%#v", reducer); strings.Contains(debug, "/tmp") {
		t.Fatalf("reducer leaked cwd: %s", debug)
	}
	candidate := testSuggestion(t, "secret", "candidate-secret", completion.SourceSpec, completion.InsertionExact)
	if debug = fmt.Sprintf("%#v", candidate); strings.Contains(debug, "candidate-secret") {
		t.Fatalf("candidate leaked content: %s", debug)
	}
}

func TestEnterRejectsPreboundaryProbeResponse(t *testing.T) {
	reducer, decoder := readyReducer(t)
	typed := reducer.RouteInput([]byte("g"))
	oldNonce, ok := findSyncNonce(typed.Effects())
	if !ok {
		t.Fatal("missing initial probe")
	}
	enter := reducer.RouteInput([]byte("\r"))
	if !bytes.Equal(forwarded(enter.Effects()), []byte("\r")) || !reducer.inputBoundaryFence {
		t.Fatalf("enter = %s", enter.Effects())
	}
	if _, active := reducer.ActiveQuery(); active {
		t.Fatal("query active behind fence")
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:1:g\x00", oldNonce)))
	if !reducer.inputBoundaryFence {
		t.Fatal("old response released fence")
	}
	for _, batch := range batches {
		if hasEffect(batch, EffectStartQuery) {
			t.Fatalf("old response started query: %s", batch)
		}
	}
}

func TestPromptAfterFencedTypeaheadRequiresCausalProbe(t *testing.T) {
	reducer, decoder := readyReducer(t)
	if got := forwarded(reducer.RouteInput([]byte("\r")).Effects()); !bytes.Equal(got, []byte("\r")) {
		t.Fatalf("enter = %q", got)
	}
	if got := forwarded(reducer.RouteInput([]byte("x")).Effects()); !bytes.Equal(got, []byte("x")) || !reducer.fencedInputPending {
		t.Fatalf("typeahead = %q, pending %t", got, reducer.fencedInputPending)
	}
	prompt := applyWire(t, reducer, decoder, []byte("prompt-ready\x00"))
	var nonce uint64
	for _, batch := range prompt {
		if value, ok := findSyncNonce(batch); ok {
			nonce = value
		}
	}
	if nonce == 0 || !reducer.inputBoundaryFence {
		t.Fatalf("prompt did not retain fence and probe: %#v", prompt)
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:1:x\x00", nonce)))
	work, _, ok := queryEffect(batches)
	if !ok || work.Query().Line() != "x" || work.Query().Cursor() != 1 || reducer.inputBoundaryFence {
		t.Fatalf("causal sync work = %s, fence %t", work, reducer.inputBoundaryFence)
	}
}

func TestEnterFencesSameBatchAndLaterActions(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("g"), "g", 1)
	present(t, reducer, generation, testSuggestion(t, "g", "git", completion.SourceSpec, completion.InsertionExact))

	sameRead := []byte("\r\x12\x10\t\x1b[Z")
	reduction := reducer.RouteInput(sameRead)
	if !bytes.Equal(forwarded(reduction.Effects()), sameRead) || !reducer.inputBoundaryFence {
		t.Fatalf("same batch = %s", reduction.Effects())
	}
	for _, kind := range []EffectKind{EffectModeChanged, EffectReplaceBuffer, EffectRefreshOverlay, EffectStartQuery, EffectRequestBufferSync} {
		if hasEffect(reduction.Effects(), kind) {
			t.Fatalf("same batch emitted %v: %s", kind, reduction.Effects())
		}
	}
	later := []byte("\x12\x10\t\x1b[Z")
	laterReduction := reducer.RouteInput(later)
	if !bytes.Equal(forwarded(laterReduction.Effects()), later) || laterReduction.Effects().Len() != 1 {
		t.Fatalf("later = %s", laterReduction.Effects())
	}
	prompt := applyWire(t, reducer, decoder, []byte("prompt-ready\x00"))
	var nonce uint64
	for _, batch := range prompt {
		if got, ok := findSyncNonce(batch); ok {
			nonce = got
		}
	}
	if nonce == 0 {
		t.Fatalf("missing causal probe: %#v", prompt)
	}
	applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:0:\x00", nonce)))
	if reducer.inputBoundaryFence {
		t.Fatal("causal response retained fence")
	}
	if toggle := reducer.RouteInput([]byte("\x12")); len(forwarded(toggle.Effects())) != 0 || reducer.Mode() != ModeHistory {
		t.Fatalf("toggle = %s, mode %s", toggle.Effects(), reducer.Mode())
	}
}

func TestQueuedBoundaryCausality(t *testing.T) {
	t.Run("queued command never receives probe", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		queued := reducer.RouteInput([]byte("\recho hi\r"))
		if !bytes.Equal(forwarded(queued.Effects()), []byte("\recho hi\r")) || reducer.queuedInputBoundaries != 2 {
			t.Fatalf("queued = %s, boundaries %d", queued.Effects(), reducer.queuedInputBoundaries)
		}
		for _, kind := range []EffectKind{EffectRequestBufferSync, EffectStartQuery} {
			if hasEffect(queued.Effects(), kind) {
				t.Fatalf("queued emitted %v", kind)
			}
		}
		lifecycle := applyWire(t, reducer, decoder, []byte("command-start:first\x00command-stop:0\x00prompt-ready\x00command-start:echo hi\x00"))
		for _, batch := range lifecycle {
			if hasEffect(batch, EffectRequestBufferSync) || hasEffect(batch, EffectStartQuery) {
				t.Fatalf("lifecycle injected work: %s", batch)
			}
		}
		if reducer.queuedInputBoundaries != 1 || reducer.shell.Foreground() != shellevents.ForegroundRunning {
			t.Fatalf("boundaries %d foreground %v", reducer.queuedInputBoundaries, reducer.shell.Foreground())
		}
		final := applyWire(t, reducer, decoder, []byte("command-stop:0\x00prompt-ready\x00"))
		for _, batch := range final {
			if hasEffect(batch, EffectRequestBufferSync) {
				t.Fatalf("final prompt injected probe: %s", batch)
			}
		}
		if reducer.inputBoundaryFence || reducer.queuedInputBoundaries != 0 {
			t.Fatalf("fence %t boundaries %d", reducer.inputBoundaryFence, reducer.queuedInputBoundaries)
		}
	})

	t.Run("forwarded input before delayed start remains fenced", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		reducer.RouteInput([]byte("first\rsecond\r"))
		first := applyWire(t, reducer, decoder, []byte("command-start:first\x00command-stop:0\x00prompt-ready\x00"))
		for _, batch := range first {
			if hasEffect(batch, EffectRequestBufferSync) {
				t.Fatalf("first lifecycle probed: %s", batch)
			}
		}
		if !reducer.fencePromptObserved {
			t.Fatal("stable prompt not noted")
		}
		typed := reducer.RouteInput([]byte("x"))
		if !bytes.Equal(forwarded(typed.Effects()), []byte("x")) || !reducer.fencedInputPending ||
			hasEffect(typed.Effects(), EffectRequestBufferSync) || hasEffect(typed.Effects(), EffectStartQuery) {
			t.Fatalf("typed = %s", typed.Effects())
		}
		final := applyWire(t, reducer, decoder, []byte("command-start:second\x00command-stop:0\x00prompt-ready\x00"))
		if len(final) != 3 || !hasEffect(final[2], EffectRequestBufferSync) || !reducer.inputBoundaryFence {
			t.Fatalf("final = %#v, fence %t", final, reducer.inputBoundaryFence)
		}
	})
}

func TestAmbiguousBoundaryHintsCollapseOnlyForLocalToggle(t *testing.T) {
	cases := map[string]struct {
		input     []byte
		lifecycle []byte
		want      uint64
	}{
		"multiline": {
			[]byte("echo \"Troy\rBarnes\"\r"),
			[]byte("command-start:echo Troy Barnes\x00command-stop:0\x00prompt-ready\x00"), 1,
		},
		"prestart ctrl-c same read": {
			[]byte("sleep 10\r\x03"),
			[]byte("command-start:sleep 10\x00command-stop:130\x00prompt-ready\x00"), 1,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			reducer, decoder := readyReducer(t)
			input := reducer.RouteInput(tc.input)
			if !bytes.Equal(forwarded(input.Effects()), tc.input) {
				t.Fatalf("forwarded = %q", forwarded(input.Effects()))
			}
			applyWire(t, reducer, decoder, tc.lifecycle)
			if reducer.queuedInputBoundaries != tc.want || !reducer.fencePromptObserved {
				t.Fatalf("boundaries %d observed %t", reducer.queuedInputBoundaries, reducer.fencePromptObserved)
			}
			toggle := reducer.RouteInput([]byte("\x12"))
			if len(forwarded(toggle.Effects())) != 0 || reducer.Mode() != ModeHistory || reducer.inputBoundaryFence {
				t.Fatalf("toggle = %s, mode %s fence %t", toggle.Effects(), reducer.Mode(), reducer.inputBoundaryFence)
			}
		})
	}

	t.Run("prestart ctrl-c later read", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		reducer.RouteInput([]byte("sleep 10\r"))
		reducer.RouteInput([]byte("\x03"))
		applyWire(t, reducer, decoder, []byte("command-start:sleep 10\x00command-stop:130\x00prompt-ready\x00"))
		if toggle := reducer.RouteInput([]byte("\x12")); len(forwarded(toggle.Effects())) != 0 || reducer.Mode() != ModeHistory || reducer.inputBoundaryFence {
			t.Fatalf("toggle = %s", toggle.Effects())
		}
	})

	t.Run("multiline plus queued command stays fenced", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		input := []byte("echo \"Troy\rBarnes\"\recho hi\r")
		reducer.RouteInput(input)
		if reducer.queuedInputBoundaries != 3 {
			t.Fatalf("boundaries = %d", reducer.queuedInputBoundaries)
		}
		batches := applyWire(t, reducer, decoder, []byte("command-start:echo Troy Barnes\x00command-stop:0\x00prompt-ready\x00command-start:echo hi\x00command-stop:0\x00prompt-ready\x00"))
		for _, batch := range batches {
			if hasEffect(batch, EffectRequestBufferSync) || hasEffect(batch, EffectStartQuery) {
				t.Fatalf("lifecycle emitted work: %s", batch)
			}
		}
		if reducer.queuedInputBoundaries != 1 || !reducer.fencePromptObserved {
			t.Fatalf("boundaries %d", reducer.queuedInputBoundaries)
		}
		if toggle := reducer.RouteInput([]byte("\x12")); len(forwarded(toggle.Effects())) != 0 || reducer.Mode() != ModeHistory {
			t.Fatalf("toggle = %s", toggle.Effects())
		}
	})
}

func TestProbeMismatchResynchronizesBeforeQuery(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("g"), "g", 1)
	ordinary := reducer.RouteInput([]byte("x"))
	expected, ok := findSyncNonce(ordinary.Effects())
	if !ok || expected != 2 {
		t.Fatalf("ordinary = %s", ordinary.Effects())
	}
	stale, _ := NewBufferReplacement("stale", 5)
	origin, _ := NewBufferReplacement("g", 1)
	reducer.pendingReplacement = &pendingReplacement{replacement: stale, kind: replacementHistoryPreview}
	reducer.historyOrigin = &origin
	reducer.historyPreviewActive = true

	var updates []shellevents.StateUpdate
	var mismatchEffects []EffectBatch
	decoder.Push([]byte("probe-buffer:b:3:2:gx\x00"), func(frame shellevents.DecodedFrame) {
		update, effects := reducer.ApplyShellFrame(frame)
		updates, mismatchEffects = append(updates, update), append(mismatchEffects, effects)
	})
	if len(updates) != 1 || updates[0].Kind() != shellevents.UpdateSnapshotRejected || !reducer.probeNeeded ||
		reducer.pendingReplacement != nil || reducer.historyOrigin != nil {
		t.Fatalf("mismatch update %v, reducer %#v", updates, reducer)
	}
	var recovery uint64
	for _, effect := range mismatchEffects[0].Effects() {
		if request, ok := effect.ProbeResyncRequest(); ok {
			recovery = request.Value()
		}
	}
	if recovery != 1 || hasEffect(mismatchEffects[0], EffectRequestBufferSync) || hasEffect(mismatchEffects[0], EffectStartQuery) {
		t.Fatalf("mismatch effects = %s", mismatchEffects[0])
	}
	wrong := applyWire(t, reducer, decoder, []byte("probe-resync:2:999\x00"))
	if len(wrong) != 1 || !wrong[0].Empty() {
		t.Fatalf("wrong recovery = %#v", wrong)
	}
	pending, ok := reducer.shell.PendingProbeResyncRequestID()
	if !ok || pending.Value() != 1 {
		t.Fatalf("pending recovery = %v, %t", pending, ok)
	}
	recovered := applyWire(t, reducer, decoder, []byte("probe-resync:1:2\x00"))
	if len(recovered) != 1 {
		t.Fatalf("recovered = %#v", recovered)
	}
	nonce, ok := findSyncNonce(recovered[0])
	if !ok || nonce != 3 || hasEffect(recovered[0], EffectStartQuery) {
		t.Fatalf("recovered = %s", recovered[0])
	}
	final := applyWire(t, reducer, decoder, []byte("probe-buffer:b:3:2:gx\x00"))
	if _, _, ok := queryEffect(final); !ok {
		t.Fatalf("final = %#v", final)
	}
}

func TestHistoryWaitsForRecoveryAfterCommandAbandonsResync(t *testing.T) {
	epoch := shellevents.InitialStreamEpoch()
	reducer, err := NewWithMode(epoch, []byte("\x12"), []byte("\x10"), []string{testProvider}, 10, []byte("/tmp"), ModeHistory)
	if err != nil {
		t.Fatal(err)
	}
	decoder := shellevents.NewDecoder(epoch)
	ready := applyWire(t, reducer, decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	work, _, ok := queryEffect(ready)
	if !ok {
		t.Fatalf("ready = %#v", ready)
	}
	oldGeneration := work.Query().Generation()
	typed := reducer.RouteInput([]byte("x"))
	if nonce, ok := findSyncNonce(typed.Effects()); !ok || nonce != 1 {
		t.Fatalf("typed = %s", typed.Effects())
	}
	mismatch := applyWire(t, reducer, decoder, []byte("probe-buffer:b:2:1:x\x00"))
	if len(mismatch) != 1 || !hasEffect(mismatch[0], EffectRequestProbeResync) {
		t.Fatalf("mismatch = %#v", mismatch)
	}
	reducer.RouteInput([]byte("\r"))
	start := applyWire(t, reducer, decoder, []byte("command-start:x\x00"))
	if len(start) != 1 || effectIndex(start[0], EffectCancelProbeResync) != 0 {
		t.Fatalf("start = %#v", start)
	}
	lifecycle := applyWire(t, reducer, decoder, []byte("command-stop:0\x00prompt-ready\x00"))
	foundRecovery := false
	for _, batch := range lifecycle {
		for _, effect := range batch.Effects() {
			if request, ok := effect.ProbeResyncRequest(); ok && request.Value() == 2 {
				foundRecovery = true
			}
		}
		if hasEffect(batch, EffectFault) || hasEffect(batch, EffectStartQuery) {
			t.Fatalf("unsafe lifecycle effect: %s", batch)
		}
	}
	if !foundRecovery || reducer.shell.SuggestionsAllowed() || !reducer.shell.ProbeResyncRequired() {
		t.Fatalf("recovery %t shell %s", foundRecovery, reducer.shell)
	}
	candidate := testSuggestion(t, "", "git status", completion.SourceHistory, completion.InsertionExact)
	if _, accepted := reducer.AcceptProviderBatch(completion.NewSuccessBatch(testProvider, oldGeneration, []completion.Suggestion{candidate})).Acceptance(); accepted {
		t.Fatal("late batch accepted during recovery")
	}
	late := reducer.ApplyRankedCandidates(oldGeneration, []completion.Suggestion{candidate})
	if hasEffect(late.Effects(), EffectFault) || hasEffect(late.Effects(), EffectReplaceBuffer) || hasEffect(late.Effects(), EffectStartQuery) {
		t.Fatalf("late effects = %s", late.Effects())
	}
	up := reducer.RouteInput([]byte("\x1b[A"))
	if !bytes.Equal(forwarded(up.Effects()), []byte("\x1b[A")) || hasEffect(up.Effects(), EffectReplaceBuffer) {
		t.Fatalf("up = %s", up.Effects())
	}
}

func TestQueuedEnterWhileProbePendingDoesNotProbe(t *testing.T) {
	reducer, decoder := readyReducer(t)
	reducer.RouteInput([]byte("\r"))
	reducer.RouteInput([]byte("x"))
	prompt := applyWire(t, reducer, decoder, []byte("prompt-ready\x00"))
	var nonce uint64
	for _, batch := range prompt {
		if value, ok := findSyncNonce(batch); ok {
			nonce = value
		}
	}
	queued := reducer.RouteInput([]byte("\r"))
	if reducer.queuedInputBoundaries != 1 || hasEffect(queued.Effects(), EffectRequestBufferSync) {
		t.Fatalf("queued = %s, boundaries %d", queued.Effects(), reducer.queuedInputBoundaries)
	}
	var update shellevents.StateUpdate
	decoder.Push([]byte(fmt.Sprintf("probe-buffer:b:%d:1:x\x00", nonce)), func(frame shellevents.DecodedFrame) {
		var effects EffectBatch
		update, effects = reducer.ApplyShellFrame(frame)
		if hasEffect(effects, EffectRequestBufferSync) {
			t.Fatalf("stale response probed: %s", effects)
		}
	})
	if update.Kind() != shellevents.UpdateSnapshotRejected || reducer.queuedInputBoundaries != 1 {
		t.Fatalf("update %v boundaries %d", update.Kind(), reducer.queuedInputBoundaries)
	}
	final := applyWire(t, reducer, decoder, []byte("command-start:x\x00command-stop:0\x00prompt-ready\x00"))
	for _, batch := range final {
		if hasEffect(batch, EffectRequestBufferSync) {
			t.Fatalf("final probed: %s", batch)
		}
	}
	if reducer.inputBoundaryFence || reducer.queuedInputBoundaries != 0 {
		t.Fatalf("fence %t boundaries %d", reducer.inputBoundaryFence, reducer.queuedInputBoundaries)
	}
}

func TestCtrlCBoundaryBehavior(t *testing.T) {
	t.Run("foreground interrupt adds no prompt boundary", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		reducer.RouteInput([]byte("sleep 10\r"))
		applyWire(t, reducer, decoder, []byte("command-start:sleep 10\x00"))
		interrupt := reducer.RouteInput([]byte("\x03"))
		if !bytes.Equal(forwarded(interrupt.Effects()), []byte("\x03")) || reducer.queuedInputBoundaries != 1 {
			t.Fatalf("interrupt = %s, boundaries %d", interrupt.Effects(), reducer.queuedInputBoundaries)
		}
		applyWire(t, reducer, decoder, []byte("command-stop:130\x00prompt-ready\x00"))
		if reducer.inputBoundaryFence {
			t.Fatal("interrupt retained fence")
		}
	})

	t.Run("toggle fenced until causal sync", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		reducer.RouteInput([]byte("\x03"))
		toggle := reducer.RouteInput([]byte("\x12"))
		if !bytes.Equal(forwarded(toggle.Effects()), []byte("\x12")) || reducer.Mode() != ModeSpec || hasEffect(toggle.Effects(), EffectModeChanged) {
			t.Fatalf("toggle = %s", toggle.Effects())
		}
		prompt := applyWire(t, reducer, decoder, []byte("prompt-ready\x00"))
		found := false
		for _, batch := range prompt {
			found = found || hasEffect(batch, EffectRequestBufferSync)
		}
		if !found || !reducer.inputBoundaryFence {
			t.Fatalf("prompt = %#v", prompt)
		}
	})

	t.Run("unacknowledged input resyncs at suppressed prompt", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		interrupted := reducer.RouteInput([]byte("x\x03"))
		if hasEffect(interrupted.Effects(), EffectRequestBufferSync) || !reducer.probeNeeded {
			t.Fatalf("interrupted = %s", interrupted.Effects())
		}
		var update shellevents.StateUpdate
		var effects EffectBatch
		decoder.Push([]byte("prompt-ready\x00"), func(frame shellevents.DecodedFrame) {
			update, effects = reducer.ApplyShellFrame(frame)
		})
		if update.Kind() != shellevents.UpdateLifecycleSuppressed || !hasEffect(effects, EffectRequestBufferSync) || hasEffect(effects, EffectStartQuery) {
			t.Fatalf("update %v effects %s", update.Kind(), effects)
		}
		nonce, _ := findSyncNonce(effects)
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:0:\x00", nonce)))
		if reducer.inputBoundaryFence {
			t.Fatal("causal empty snapshot retained fence")
		}
		if _, active := reducer.ActiveQuery(); active {
			t.Fatal("spec query started for empty buffer")
		}
	})
}

func TestShiftTabFallbackAndConfiguredBinding(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("g"), "g", 1)
	present(t, reducer, generation, testSuggestion(t, "g", "git", completion.SourceSpec, completion.InsertionExact))
	fallback := reducer.RouteInput([]byte("\x1b[Z"))
	selectionState := reducer.Selection()
	if !bytes.Equal(forwarded(fallback.Effects()), []byte("\x1b[Z")) || !selectionState.LayerEnabled() || hasEffect(fallback.Effects(), EffectRefreshOverlay) {
		t.Fatalf("fallback = %s", fallback.Effects())
	}

	epoch := shellevents.InitialStreamEpoch()
	configured, err := New(epoch, []byte("\x12"), []byte("\x1b[Z"), []string{testProvider}, 10, []byte("/tmp"))
	if err != nil {
		t.Fatal(err)
	}
	configuredDecoder := shellevents.NewDecoder(epoch)
	applyWire(t, configured, configuredDecoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	handled := configured.RouteInput([]byte("\x1b[Z"))
	configuredSelection := configured.Selection()
	if len(forwarded(handled.Effects())) != 0 || configuredSelection.LayerEnabled() {
		t.Fatalf("handled = %s", handled.Effects())
	}
}

func TestHistoryRestoreByToggleAndEscape(t *testing.T) {
	t.Run("toggle restores acknowledged preview", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		nonce := requestHistoryPreview(t, reducer, "git", "git status")
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", nonce)))
		restore := reducer.RouteInput([]byte("\x12"))
		if reducer.Mode() != ModeSpec || !hasEffect(restore.Effects(), EffectReplaceBuffer) {
			t.Fatalf("restore = %s", restore.Effects())
		}
		replacement, _ := restore.Effects().Effects()[effectIndex(restore.Effects(), EffectReplaceBuffer)].Replacement()
		if replacement.Text() != "git" || replacement.Cursor() != 3 {
			t.Fatalf("replacement = %s", replacement)
		}
	})

	t.Run("escape forwards only after restore and sync", func(t *testing.T) {
		reducer, decoder := readyReducer(t)
		synchronize(t, reducer, decoder, []byte("git"), "git", 3)
		previewNonce := requestHistoryPreview(t, reducer, "git", "git status")
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:10:git status\x00", previewNonce)))
		if pending := reducer.RouteInput([]byte("\x1b")); !pending.Effects().Empty() {
			t.Fatalf("pending escape = %s", pending.Effects())
		}
		unwind := reducer.FlushInput()
		replace, sync, escape := effectIndex(unwind.Effects(), EffectReplaceBuffer), effectIndex(unwind.Effects(), EffectRequestBufferSync), effectIndex(unwind.Effects(), EffectForwardInput)
		if replace < 0 || replace >= sync || sync >= escape || reducer.Mode() != ModeSpec || !reducer.ReplacementPending() {
			t.Fatalf("unwind = %s", unwind.Effects())
		}
		nonce, _ := findSyncNonce(unwind.Effects())
		applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:3:git\x00", nonce)))
		query, ok := reducer.ActiveQuery()
		if !ok || query.Line() != "git" || reducer.ReplacementPending() {
			t.Fatalf("query = %s, pending %t", query, reducer.ReplacementPending())
		}
	})
}

func TestReconfigureDefersRetainedBindingAndPreservesAuthority(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	// Escape is a retained prefix because configured and fixed sequences share it.
	reducer.RouteInput([]byte("\x1b"))
	if effects, applied, err := reducer.Reconfigure([]byte("\x11"), []byte("\x10"), 5); err != nil || applied || !effects.Empty() {
		t.Fatalf("reconfigure = %s, applied %t, err %v", effects, applied, err)
	}
	flushed := reducer.FlushInput()
	effects, applied, err := reducer.Reconfigure([]byte("\x11"), []byte("\x10"), 5)
	if err != nil || !applied || hasEffect(effects, EffectStartQuery) || !reducer.QueryRestartDeferred() {
		t.Fatalf("retry = %s, applied %t, deferred %t, err %v", effects, applied, reducer.QueryRestartDeferred(), err)
	}
	nonce, ok := findSyncNonce(flushed.Effects())
	if !ok {
		t.Fatalf("flush did not request authority: %s", flushed.Effects())
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:3:git\x00", nonce)))
	if _, _, ok := queryEffect(batches); !ok {
		t.Fatalf("authority did not restart reconfigured query: %#v", batches)
	}
	oldToggle := reducer.RouteInput([]byte("\x12"))
	if !bytes.Equal(forwarded(oldToggle.Effects()), []byte("\x12")) {
		t.Fatalf("old binding = %s", oldToggle.Effects())
	}
}

func TestInputBoundaryExhaustionDoesNotWrap(t *testing.T) {
	reducer, _ := readyReducer(t)
	reducer.inputBoundaryFence = true
	reducer.queuedInputBoundaries = ^uint64(0)
	reduction := reducer.RouteInput([]byte("\r"))
	if reducer.queuedInputBoundaries != ^uint64(0) || !hasEffect(reduction.Effects(), EffectFault) || !reducer.fencedInputPending {
		t.Fatalf("reduction = %s, boundaries %d", reduction.Effects(), reducer.queuedInputBoundaries)
	}
	for _, effect := range reduction.Effects().Effects() {
		if fault, ok := effect.Fault(); ok && fault.Kind() != FaultInputBoundaryExhausted {
			t.Fatalf("fault = %v", fault.Kind())
		}
	}
}

func TestEmptyPromptUpRecallsHistoryAndPreviewsResult(t *testing.T) {
	reducer, _ := readyReducer(t)
	up := reducer.RouteInput([]byte("\x1b[A"))
	if len(forwarded(up.Effects())) != 0 || reducer.Mode() != ModeHistory || !hasEffect(up.Effects(), EffectModeChanged) {
		t.Fatalf("up = %s, mode %s", up.Effects(), reducer.Mode())
	}
	work, _, ok := queryEffect([]EffectBatch{up.Effects()})
	if !ok || work.Query().Line() != "" {
		t.Fatalf("history work = %s", work)
	}
	candidate := testSuggestion(t, "", "git status", completion.SourceHistory, completion.InsertionExact)
	presentEffects := reducer.AcceptProviderBatch(completion.NewSuccessBatch(testProvider, work.Query().Generation(), []completion.Suggestion{candidate}))
	if _, ok := presentEffects.Acceptance(); !ok {
		t.Fatal("history provider batch rejected")
	}
	presentation := reducer.ApplyRankedCandidates(work.Query().Generation(), []completion.Suggestion{candidate})
	if !hasEffect(presentation.Effects(), EffectReplaceBuffer) || !hasEffect(presentation.Effects(), EffectRequestBufferSync) {
		t.Fatalf("history recall did not preview: %s", presentation.Effects())
	}
}

func TestMenuToggleAndNavigationDoNotWrap(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("g"), "g", 1)
	first := testSuggestion(t, "g", "git", completion.SourceSpec, completion.InsertionExact)
	second := testSuggestion(t, "g", "grep", completion.SourceSpec, completion.InsertionExact)
	if _, ok := reducer.AcceptProviderBatch(completion.NewSuccessBatch(testProvider, generation, []completion.Suggestion{first, second})).Acceptance(); !ok {
		t.Fatal("provider batch rejected")
	}
	if got := reducer.ApplyRankedCandidates(generation, []completion.Suggestion{first, second}).Outcome().Kind(); got != coordinator.PresentationApplied {
		t.Fatalf("presentation = %v", got)
	}
	for range 3 {
		down := reducer.RouteInput([]byte("\x1b[B"))
		if len(forwarded(down.Effects())) != 0 || !hasEffect(down.Effects(), EffectRefreshOverlay) {
			t.Fatalf("down = %s", down.Effects())
		}
	}
	selectionState := reducer.Selection()
	if index, ok := selectionState.SelectedIndex(); !ok || index != 1 {
		t.Fatalf("selected = %d, %t", index, ok)
	}
	menu := reducer.RouteInput([]byte("\x10"))
	selectionState = reducer.Selection()
	if len(forwarded(menu.Effects())) != 0 || selectionState.LayerEnabled() || !hasEffect(menu.Effects(), EffectClearOverlay) {
		t.Fatalf("menu = %s", menu.Effects())
	}
	if tab := reducer.RouteInput([]byte("\t")); !bytes.Equal(forwarded(tab.Effects()), []byte("\t")) || hasEffect(tab.Effects(), EffectReplaceBuffer) {
		t.Fatalf("disabled tab = %s", tab.Effects())
	}
}

func TestCancelledAliasExpansionIsIgnored(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("gs "), "gs ", 3)
	edit, _ := completion.NewTextEdit(0, 2, "git status")
	reducer.ObserveShellOutput()
	if effects := reducer.ApplyAliasExpansion(generation, edit); !effects.Empty() || reducer.ReplacementPending() {
		t.Fatalf("cancelled expansion = %s", effects)
	}
}

func TestShellCWDUpdateDefersUntilFreshBufferAuthority(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	cwd := applyWire(t, reducer, decoder, []byte("cwd:/var/tmp\x00"))
	if !bytes.Equal(reducer.CWD(), []byte("/var/tmp")) || !reducer.QueryRestartDeferred() || len(cwd) != 1 || hasEffect(cwd[0], EffectStartQuery) {
		t.Fatalf("cwd effects = %#v, cwd %q, deferred %t", cwd, reducer.CWD(), reducer.QueryRestartDeferred())
	}
	typed := reducer.RouteInput([]byte("x"))
	nonce, ok := findSyncNonce(typed.Effects())
	if !ok {
		t.Fatalf("typed = %s", typed.Effects())
	}
	batches := applyWire(t, reducer, decoder, []byte(fmt.Sprintf("probe-buffer:b:%d:4:gitx\x00", nonce)))
	work, _, ok := queryEffect(batches)
	if !ok || !bytes.Equal(work.Query().CWD(), []byte("/var/tmp")) || work.Query().Line() != "gitx" || reducer.QueryRestartDeferred() {
		t.Fatalf("work = %s, deferred %t", work, reducer.QueryRestartDeferred())
	}
}

func TestVisibleGhostAcceptanceReplacesWithoutForwardingArrow(t *testing.T) {
	reducer, decoder := readyReducer(t)
	generation := synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	present(t, reducer, generation, testSuggestion(t, "git", "git status", completion.SourceSpec, completion.InsertionExact))
	accepted := reducer.RouteInput([]byte("\x1b[C"))
	if len(forwarded(accepted.Effects())) != 0 || !hasEffect(accepted.Effects(), EffectReplaceBuffer) || !hasEffect(accepted.Effects(), EffectRequestBufferSync) {
		t.Fatalf("accepted = %s", accepted.Effects())
	}
	replacement, _ := accepted.Effects().Effects()[effectIndex(accepted.Effects(), EffectReplaceBuffer)].Replacement()
	if replacement.Text() != "git status" || replacement.Cursor() != len("git status") {
		t.Fatalf("replacement = %s", replacement)
	}
}

func TestCloseCancelsActiveQuery(t *testing.T) {
	reducer, decoder := readyReducer(t)
	synchronize(t, reducer, decoder, []byte("git"), "git", 3)
	query, ok := reducer.ActiveQuery()
	if !ok {
		t.Fatal("missing active query")
	}
	// Capture the observer-only token from the original StartQuery effect by
	// restarting the same authority through a CWD update.
	effects, err := reducer.UpdateCWD([]byte("/var/tmp"))
	if err != nil {
		t.Fatal(err)
	}
	work, _, ok := queryEffect([]EffectBatch{effects})
	if !ok || work.Query().Generation() == query.Generation() {
		t.Fatal("missing restarted query")
	}
	if err := reducer.Close(); err != nil {
		t.Fatal(err)
	}
	if !work.Cancellation().IsCancelled() {
		t.Fatal("close did not cancel provider work")
	}
	if err := reducer.Close(); err != nil {
		t.Fatal(err)
	}
}
