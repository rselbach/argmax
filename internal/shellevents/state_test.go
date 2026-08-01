package shellevents

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/shellcontrol"
)

func applyWire(t *testing.T, state *ShellSessionState, decoder *Decoder, wire []byte) []StateUpdate {
	t.Helper()
	frames := decodeEvents(decoder, wire)
	updates := make([]StateUpdate, 0, len(frames))
	for _, frame := range frames {
		updates = append(updates, state.Apply(frame))
	}
	return updates
}

func requireBuffer(t *testing.T, state *ShellSessionState, want []byte) BufferSnapshot {
	t.Helper()
	buffer, ok := state.Buffer()
	if !ok {
		t.Fatalf("Buffer() ok = false, want %q", want)
	}
	if !bytes.Equal(buffer.Bytes(), want) {
		t.Fatalf("Buffer().Bytes() = %q, want %q", buffer.Bytes(), want)
	}
	return buffer
}

func requireProbeError(t *testing.T, err error, want ProbeRequestErrorKind) {
	t.Helper()
	var probeErr *ProbeRequestError
	if !errors.As(err, &probeErr) || probeErr.Kind() != want {
		t.Fatalf("probe error = %T %v, want kind %d", err, err, want)
	}
}

func requireResyncError(t *testing.T, err error, want ProbeResyncRequestErrorKind) {
	t.Helper()
	var resyncErr *ProbeResyncRequestError
	if !errors.As(err, &resyncErr) || resyncErr.Kind() != want {
		t.Fatalf("resync error = %T %v, want kind %d", err, err, want)
	}
}

func mustObserveLocalInput(t *testing.T, state *ShellSessionState) InputGeneration {
	t.Helper()
	generation, err := state.ObserveLocalInput()
	if err != nil {
		t.Fatalf("ObserveLocalInput(): %v", err)
	}
	return generation
}

func mustBeginSyncProbe(t *testing.T, state *ShellSessionState) SnapshotNonce {
	t.Helper()
	nonce, err := state.BeginSyncProbe()
	if err != nil {
		t.Fatalf("BeginSyncProbe(): %v", err)
	}
	return nonce
}

func TestStateRejectedFrameDesynchronizesAndPromptRecovers(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:native-buffer\x00prompt-ready\x00wat\x00prompt-ready\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	if update := state.Apply(frames[0]); update.Kind() != UpdateCapabilityChanged {
		t.Fatalf("capability update = %d", update.Kind())
	}
	if update := state.Apply(frames[1]); update.Kind() != UpdatePromptReady {
		t.Fatalf("prompt update = %d", update.Kind())
	} else if recovered, ok := update.Recovered(); !ok || !recovered {
		t.Errorf("Recovered() = (%t, %t), want true, true", recovered, ok)
	}
	if !state.SuggestionsAllowed() {
		t.Fatal("SuggestionsAllowed() = false after prompt")
	}
	update := state.Apply(frames[2])
	if update.Kind() != UpdateFrameRejected {
		t.Fatalf("rejected update = %d", update.Kind())
	}
	frameErr, ok := update.FrameError()
	if !ok || frameErr.Kind() != FrameUnknownEvent {
		t.Errorf("FrameError() = (%v, %t), want unknown event", frameErr, ok)
	}
	if state.Synchronization() != StateDesynchronized || state.SuggestionsAllowed() {
		t.Error("rejection did not invalidate suggestions")
	}
	last, ok := state.LastPosition()
	if !ok || last != frames[2].Position() {
		t.Errorf("LastPosition() = (%v, %t), want rejected frame position", last, ok)
	}
	if update = state.Apply(frames[3]); update.Kind() != UpdatePromptReady {
		t.Fatalf("recovery update = %d", update.Kind())
	} else if recovered, _ := update.Recovered(); !recovered {
		t.Error("prompt did not report recovery")
	}
	if !state.SuggestionsAllowed() {
		t.Error("SuggestionsAllowed() = false after recovery")
	}
}

func TestStateControlEventsProduceTypedUpdatesWithoutChangingAuthority(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	updates := applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"cwd:/tmp/Greendale Community College\x00reload-request:42\x00"))
	directory, ok := updates[2].WorkingDirectory()
	if !ok || !bytes.Equal(directory.Bytes(), []byte("/tmp/Greendale Community College")) {
		t.Errorf("working-directory update = (%q, %t)", directory.Bytes(), ok)
	}
	request, ok := updates[3].ReloadRequest()
	if !ok || request.Nonce() != 42 {
		t.Errorf("reload update = (%d, %t), want nonce 42", request.Nonce(), ok)
	}
	if !state.SuggestionsAllowed() {
		t.Error("control events unexpectedly changed editing authority")
	}
}

func TestStateRejectionDuringCommandInvalidatesAttribution(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	updates := applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"buffer:b:7:echo hi\x00command-start:echo hi\x00wat\x00command-stop:0\x00"+
		"prompt-ready\x00buffer:b:1:x\x00"))
	if updates[4].Kind() != UpdateFrameRejected || updates[5].Kind() != UpdateLifecycleSuppressed {
		t.Errorf("fault updates = (%d, %d), want frame rejected then lifecycle suppressed", updates[4].Kind(), updates[5].Kind())
	}
	for index, update := range updates {
		if update.Kind() == UpdateCommandStopped {
			t.Errorf("update %d emitted an attributed completion after rejection", index)
		}
	}
	if updates[7].Kind() != UpdateBufferSynchronized {
		t.Errorf("final update = %d, want buffer synchronized", updates[7].Kind())
	} else if recovered, _ := updates[7].Recovered(); recovered {
		t.Error("final native buffer reported recovery after prompt already recovered")
	}
	requireBuffer(t, state, []byte("x"))
	if !state.SuggestionsAllowed() {
		t.Error("suggestions remain disabled after snapshot")
	}
}

func TestStateExactPreexecAttribution(t *testing.T) {
	tests := map[string]struct {
		wire           string
		wantCommand    []byte
		wantMatch      bool
		hasMatch       bool
		wantAttributed bool
		wantStopKind   StateUpdateKind
	}{
		"mismatch supersedes snapshot": {
			wire: "capability:native-buffer\x00prompt-ready\x00buffer:b:3:git status\x00" +
				"command-start:git diff\x00command-stop:1\x00",
			wantCommand: []byte("git diff"), wantMatch: false, hasMatch: true,
			wantAttributed: true, wantStopKind: UpdateCommandStopped,
		},
		"matching snapshot": {
			wire: "capability:native-buffer\x00prompt-ready\x00buffer:b:7:echo hi\x00" +
				"command-start:echo hi\x00command-stop:0\x00",
			wantCommand: []byte("echo hi"), wantMatch: true, hasMatch: true,
			wantAttributed: true, wantStopKind: UpdateCommandStopped,
		},
		"exact fallback without snapshot": {
			wire: "capability:native-buffer\x00prompt-ready\x00" +
				"command-start:echo hi\x00command-stop:0\x00",
			wantCommand: []byte("echo hi"), hasMatch: false,
			wantAttributed: true, wantStopKind: UpdateCommandStopped,
		},
		"unknown start never attributes": {
			wire: "capability:native-buffer\x00prompt-ready\x00buffer:b:10:git status\x00" +
				"command-start-unknown\x00command-stop:1\x00",
			wantAttributed: false, wantStopKind: UpdateCommandStoppedWithoutAttribution,
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			decoder := testDecoder()
			state := NewShellSessionState(InitialStreamEpoch())
			updates := applyWire(t, state, decoder, []byte(tc.wire))
			start := updates[len(updates)-2]
			if start.Kind() != UpdateCommandStarted {
				t.Fatalf("start update = %d, want UpdateCommandStarted", start.Kind())
			}
			details, _ := start.CommandStart()
			_, hasSource := details.Source()
			if hasSource != tc.wantAttributed {
				t.Errorf("source presence = %t, want %t", hasSource, tc.wantAttributed)
			}
			match, hasMatch := details.PreexecMatchesSnapshot()
			if hasMatch != tc.hasMatch || hasMatch && match != tc.wantMatch {
				t.Errorf("PreexecMatchesSnapshot() = (%t, %t), want (%t, %t)", match, hasMatch, tc.wantMatch, tc.hasMatch)
			}
			stop := updates[len(updates)-1]
			if stop.Kind() != tc.wantStopKind {
				t.Fatalf("stop update = %d, want %d", stop.Kind(), tc.wantStopKind)
			}
			if tc.wantAttributed {
				completed, ok := stop.CompletedCommand()
				if !ok || !bytes.Equal(completed.Command().Bytes(), tc.wantCommand) {
					t.Fatalf("CompletedCommand() = (%s, %t), want %q", completed, ok, tc.wantCommand)
				}
				if completed.Source() != AttributionLifecycleFrame {
					t.Errorf("source = %d, want lifecycle", completed.Source())
				}
			} else {
				status, ok := stop.Status()
				if !ok || status.Value() != 1 {
					t.Errorf("unattributed status = (%d, %t), want 1", status.Value(), ok)
				}
			}
		})
	}
}

func TestStateUnknownStartWithoutProbeNeverCompletesFalsely(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	updates := applyWire(t, state, decoder, []byte("capability:unavailable\x00prompt-ready\x00"+
		"command-start-unknown\x00command-stop:0\x00"))
	if updates[3].Kind() != UpdateCommandStoppedWithoutAttribution {
		t.Fatalf("stop update = %d, want unattributed", updates[3].Kind())
	}
	status, _ := updates[3].Status()
	if !status.Success() {
		t.Errorf("status = %d, want success", status.Value())
	}
	if state.SuggestionsAllowed() {
		t.Error("SuggestionsAllowed() = true with unavailable adapter")
	}
}

func TestStateDuplicateAndImpossibleLifecycleNeverComplete(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	updates := applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"command-start:echo hi\x00command-start:echo bye\x00command-stop:0\x00"+
		"prompt-ready\x00command-stop:0\x00"))
	if lifecycle, ok := updates[3].LifecycleError(); !ok || lifecycle != LifecycleDuplicateCommandStart {
		t.Errorf("duplicate update = (%d, %t), want duplicate start", lifecycle, ok)
	}
	if updates[4].Kind() != UpdateLifecycleSuppressed {
		t.Errorf("stop after fault = %d, want suppressed", updates[4].Kind())
	}
	if lifecycle, ok := updates[6].LifecycleError(); !ok || lifecycle != LifecycleCommandStopWithoutStart {
		t.Errorf("idle stop update = (%d, %t), want stop without start", lifecycle, ok)
	}
	for _, update := range updates {
		if update.Kind() == UpdateCommandStopped {
			t.Error("impossible lifecycle emitted attributed completion")
		}
	}
}

func TestStateStreamOrderFaultRequiresExplicitReset(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"buffer:b:3:old\x00buffer:b:3:new\x00prompt-ready\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	update := state.Apply(frames[3])
	if update.Kind() != UpdateStreamOrderRejected {
		t.Fatalf("skipped-frame update = %d, want order rejection", update.Kind())
	}
	order, _ := update.StreamOrderRejection()
	last, ok := order.LastAccepted()
	if !ok || last != frames[1].Position() || order.Received() != frames[3].Position() {
		t.Errorf("order rejection = (%v, %v, %t)", order.Received(), last, ok)
	}
	if _, ok := state.Buffer(); ok || state.Synchronization() != StateDesynchronized {
		t.Error("order fault retained authoritative buffer")
	}
	for _, frame := range []DecodedFrame{frames[2], frames[4]} {
		if got := state.Apply(frame).Kind(); got != UpdateStreamOrderRejected {
			t.Errorf("post-fault update = %d, want persistent order rejection", got)
		}
	}
}

func TestStateExplicitEpochResetPreventsRecreationStaleness(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"))
	epoch, err := decoder.ResetStream()
	if err != nil {
		t.Fatalf("decoder.ResetStream(): %v", err)
	}
	if err := state.ResetStream(epoch); err != nil {
		t.Fatalf("state.ResetStream(): %v", err)
	}
	if state.Capability() != BufferSyncUnknown || state.SuggestionsAllowed() {
		t.Error("reset retained capability authority")
	}
	updates := applyWire(t, state, decoder, []byte("prompt-ready\x00capability:native-buffer\x00"+
		"prompt-ready\x00buffer:b:1:x\x00"))
	if updates[0].Kind() != UpdateLifecycleSuppressed {
		t.Errorf("pre-handshake prompt = %d, want suppressed", updates[0].Kind())
	}
	if updates[3].Kind() != UpdateBufferSynchronized {
		t.Errorf("final snapshot update = %d", updates[3].Kind())
	} else if recovered, _ := updates[3].Recovered(); recovered {
		t.Error("snapshot reported recovery after prompt")
	}
	requireBuffer(t, state, []byte("x"))

	if err := state.ResetStream(epoch); err == nil {
		t.Fatal("non-increasing ResetStream() error = nil")
	} else if resetErr, ok := err.(*StreamResetError); !ok || resetErr.Kind() != StreamResetNonIncreasingEpoch {
		t.Errorf("ResetStream() error = %T %v", err, err)
	}
}

func TestStateHigherEpochRequiresMatchingReset(t *testing.T) {
	decoder := testDecoder()
	epoch, _ := decoder.ResetStream()
	frame := decodeEvents(decoder, []byte("capability:unavailable\x00"))[0]
	state := NewShellSessionState(InitialStreamEpoch())
	if update := state.Apply(frame); update.Kind() != UpdateStreamOrderRejected {
		t.Errorf("without reset update = %d, want order rejection", update.Kind())
	}
	if state.Epoch() != InitialStreamEpoch() {
		t.Error("wrong-epoch frame changed reducer epoch")
	}
	if err := state.ResetStream(epoch); err != nil {
		t.Fatalf("ResetStream(): %v", err)
	}
	if update := state.Apply(frame); update.Kind() != UpdateCapabilityChanged {
		t.Errorf("after reset update = %d, want capability", update.Kind())
	}
}

func TestStateSnapshotCapabilityCorrelationFailsClosed(t *testing.T) {
	tests := map[string]struct {
		wire string
		want SnapshotRejectionKind
	}{
		"probe adapter requires nonce": {
			wire: "capability:sync-probe:0\x00prompt-ready\x00buffer:b:1:x\x00",
			want: SnapshotMissingProbeNonce,
		},
		"native adapter rejects nonce": {
			wire: "capability:native-buffer\x00prompt-ready\x00probe-buffer:b:1:1:x\x00",
			want: SnapshotUnexpectedProbeNonce,
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			decoder := testDecoder()
			state := NewShellSessionState(InitialStreamEpoch())
			updates := applyWire(t, state, decoder, []byte(tc.wire))
			rejection, ok := updates[2].SnapshotRejection()
			if !ok || rejection.Kind() != tc.want {
				t.Errorf("SnapshotRejection() = (%v, %t), want kind %d", rejection, ok, tc.want)
			}
			if _, ok := state.Buffer(); ok || state.SuggestionsAllowed() {
				t.Error("capability-mismatched snapshot retained authority")
			}
		})
	}
}

func TestStateStaleProbeResponseAfterNewInputFailsClosed(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"+
		"probe-buffer:b:1:1:x\x00probe-buffer:b:2:1:y\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	requested, err := state.ObserveLocalInput()
	if err != nil {
		t.Fatalf("ObserveLocalInput(): %v", err)
	}
	nonce, err := state.BeginSyncProbe()
	if err != nil || nonce.Value() != 1 {
		t.Fatalf("BeginSyncProbe() = (%d, %v), want 1", nonce.Value(), err)
	}
	current, err := state.ObserveLocalInput()
	if err != nil {
		t.Fatalf("second ObserveLocalInput(): %v", err)
	}
	update := state.Apply(frames[2])
	rejection, ok := update.SnapshotRejection()
	if !ok || rejection.Kind() != SnapshotStaleProbeGeneration ||
		rejection.RequestedGeneration() != requested || rejection.CurrentGeneration() != current {
		t.Errorf("stale update = (%v, %t)", rejection, ok)
	}
	confirmed, ok := state.ConfirmedProbeNonce()
	if !ok || confirmed.Value() != 1 {
		t.Errorf("stale matching response did not confirm baseline: (%d, %t)", confirmed.Value(), ok)
	}
	if _, ok := state.Buffer(); ok || state.SuggestionsAllowed() {
		t.Error("stale response became authoritative")
	}
	nonce, err = state.BeginSyncProbe()
	if err != nil || nonce.Value() != 2 {
		t.Fatalf("second BeginSyncProbe() = (%d, %v), want 2", nonce.Value(), err)
	}
	if update = state.Apply(frames[3]); update.Kind() != UpdateBufferSynchronized {
		t.Fatalf("fresh response update = %d", update.Kind())
	} else if recovered, _ := update.Recovered(); !recovered {
		t.Error("fresh response did not report recovery")
	}
	requireBuffer(t, state, []byte("y"))
	generation, ok := state.BufferGeneration()
	if !ok || generation != current {
		t.Errorf("BufferGeneration() = (%v, %t), want current", generation, ok)
	}
}

func TestStateDelayedPromptCannotOverwriteNewInputAuthority(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00prompt-ready\x00"+
		"probe-buffer:b:1:1:x\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	generation, _ := state.ObserveLocalInput()
	if _, err := state.BeginSyncProbe(); err != nil {
		t.Fatalf("BeginSyncProbe(): %v", err)
	}
	if update := state.Apply(frames[2]); update.Kind() != UpdateLifecycleSuppressed {
		t.Errorf("delayed prompt update = %d, want suppressed", update.Kind())
	}
	if state.InputGeneration() != generation {
		t.Error("delayed prompt changed input generation")
	}
	if _, ok := state.Buffer(); ok {
		t.Error("delayed prompt installed empty buffer")
	}
	pending, ok := state.PendingProbeNonce()
	if !ok || pending.Value() != 1 {
		t.Errorf("PendingProbeNonce() = (%d, %t), want 1", pending.Value(), ok)
	}
	if update := state.Apply(frames[3]); update.Kind() != UpdateBufferSynchronized {
		t.Errorf("probe update = %d", update.Kind())
	}
	requireBuffer(t, state, []byte("x"))
}

func TestStateHigherProbeNonceRequiresCorrelatedResync(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:10\x00prompt-ready\x00"+
		"probe-buffer:b:18446744073709551615:1:x\x00probe-resync:2:999\x00"+
		"probe-resync:1:3\x00probe-buffer:b:4:1:y\x00probe-resync:1:777\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	nonce, err := state.BeginSyncProbe()
	if err != nil || nonce.Value() != 11 {
		t.Fatalf("BeginSyncProbe() = (%d, %v), want 11", nonce.Value(), err)
	}
	update := state.Apply(frames[2])
	rejection, ok := update.SnapshotRejection()
	expected, hasExpected := rejection.ExpectedNonce()
	if !ok || rejection.Kind() != SnapshotProbeNonceMismatch || !hasExpected ||
		expected.Value() != 11 || rejection.Nonce().Value() != math.MaxUint64 {
		t.Fatalf("higher mismatch = (%v, %t)", rejection, ok)
	}
	confirmed, _ := state.ConfirmedProbeNonce()
	if confirmed.Value() != 10 || !state.ProbeResyncRequired() {
		t.Errorf("mismatch baseline = %d, resync = %t", confirmed.Value(), state.ProbeResyncRequired())
	}
	if _, err := state.BeginSyncProbe(); err == nil {
		t.Fatal("BeginSyncProbe() during required resync succeeded")
	} else {
		requireProbeError(t, err, ProbeResyncRequired)
	}
	request, err := state.BeginProbeResync()
	if err != nil || request.Value() != 1 {
		t.Fatalf("BeginProbeResync() = (%d, %v), want 1", request.Value(), err)
	}
	update = state.Apply(frames[3])
	wrong, ok := update.ProbeResyncRejection()
	wantWrong, _ := shellcontrol.NewProbeResyncRequestID(2)
	wantExpected, _ := wrong.ExpectedRequestID()
	if !ok || wantExpected != request || wrong.ReceivedRequestID() != wantWrong {
		t.Errorf("wrong response = (%v, %t)", wrong, ok)
	}
	pending, ok := state.PendingProbeResyncRequestID()
	if !ok || pending != request {
		t.Error("wrong response consumed pending request")
	}
	update = state.Apply(frames[4])
	response, ok := update.ProbeResync()
	if !ok || response.RequestID() != request || response.LastProbeNonce().Value() != 3 {
		t.Fatalf("matching response = (%v, %t)", response, ok)
	}
	confirmed, _ = state.ConfirmedProbeNonce()
	if confirmed.Value() != 3 || state.ProbeResyncRequired() {
		t.Errorf("recovered baseline = %d, resync required = %t", confirmed.Value(), state.ProbeResyncRequired())
	}
	if _, ok := state.Buffer(); ok {
		t.Error("resync response installed a buffer")
	}
	nonce, err = state.BeginSyncProbe()
	if err != nil || nonce.Value() != 4 {
		t.Fatalf("post-resync probe = (%d, %v), want 4", nonce.Value(), err)
	}
	if update = state.Apply(frames[5]); update.Kind() != UpdateBufferSynchronized {
		t.Fatalf("fresh probe update = %d", update.Kind())
	}
	requireBuffer(t, state, []byte("y"))
	update = state.Apply(frames[6])
	wrong, ok = update.ProbeResyncRejection()
	_, hasExpected = wrong.ExpectedRequestID()
	if !ok || hasExpected || wrong.ReceivedRequestID() != request {
		t.Errorf("unsolicited old response = (%v, %t)", wrong, ok)
	}
	requireBuffer(t, state, []byte("y"))
}

func TestStateUnsolicitedProbeNonceHandling(t *testing.T) {
	tests := map[string]struct {
		nonce         string
		wantResync    bool
		wantConfirmed uint64
	}{
		"higher requires resync": {nonce: "18446744073709551615", wantResync: true, wantConfirmed: 5},
		"equal does not":         {nonce: "5", wantConfirmed: 5},
		"lower does not":         {nonce: "4", wantConfirmed: 5},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			decoder := testDecoder()
			state := NewShellSessionState(InitialStreamEpoch())
			updates := applyWire(t, state, decoder, []byte("capability:sync-probe:5\x00prompt-ready\x00"+
				"probe-buffer:b:"+tc.nonce+":1:x\x00"))
			rejection, ok := updates[2].SnapshotRejection()
			_, hasExpected := rejection.ExpectedNonce()
			if !ok || rejection.Kind() != SnapshotProbeNonceMismatch || hasExpected {
				t.Errorf("rejection = (%v, %t), want unsolicited mismatch", rejection, ok)
			}
			confirmed, _ := state.ConfirmedProbeNonce()
			if confirmed.Value() != tc.wantConfirmed || state.ProbeResyncRequired() != tc.wantResync {
				t.Errorf("state = baseline %d, resync %t; want %d, %t", confirmed.Value(), state.ProbeResyncRequired(), tc.wantConfirmed, tc.wantResync)
			}
		})
	}
}

func TestStateLowerProbeMismatchRetainsPendingRequest(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:10\x00prompt-ready\x00"+
		"probe-buffer:b:9:1:x\x00probe-buffer:b:11:1:y\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	mustBeginSyncProbe(t, state)
	update := state.Apply(frames[2])
	rejection, ok := update.SnapshotRejection()
	expected, hasExpected := rejection.ExpectedNonce()
	if !ok || rejection.Kind() != SnapshotProbeNonceMismatch || !hasExpected || expected.Value() != 11 || rejection.Nonce().Value() != 9 {
		t.Errorf("lower mismatch = (%v, %t)", rejection, ok)
	}
	pending, ok := state.PendingProbeNonce()
	if !ok || pending.Value() != 11 || state.ProbeResyncRequired() {
		t.Error("lower mismatch consumed pending probe or required resync")
	}
	if update = state.Apply(frames[3]); update.Kind() != UpdateBufferSynchronized {
		t.Errorf("matching response = %d, want synchronized", update.Kind())
	}
	confirmed, _ := state.ConfirmedProbeNonce()
	if confirmed.Value() != 11 {
		t.Errorf("confirmed nonce = %d, want 11", confirmed.Value())
	}
}

func TestStateCapabilityAndStreamResetClearProbeRecovery(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:1\x00prompt-ready\x00probe-buffer:b:3:1:x\x00"+
		"capability:sync-probe:7\x00prompt-ready\x00probe-buffer:b:9:1:y\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	mustBeginSyncProbe(t, state)
	state.Apply(frames[2])
	first, err := state.BeginProbeResync()
	if err != nil || first.Value() != 1 {
		t.Fatalf("first recovery = (%d, %v)", first.Value(), err)
	}
	state.Apply(frames[3])
	confirmed, ok := state.ConfirmedProbeNonce()
	if !ok || confirmed.Value() != 7 || state.ProbeResyncRequired() {
		t.Errorf("capability reset state = (%d, %t, %t)", confirmed.Value(), ok, state.ProbeResyncRequired())
	}
	if _, ok := state.PendingProbeResyncRequestID(); ok {
		t.Error("capability announcement retained pending recovery")
	}
	state.Apply(frames[4])
	mustObserveLocalInput(t, state)
	mustBeginSyncProbe(t, state)
	state.Apply(frames[5])
	second, err := state.BeginProbeResync()
	if err != nil || second.Value() != 2 {
		t.Fatalf("second recovery = (%d, %v), want ID 2", second.Value(), err)
	}
	epoch, _ := decoder.ResetStream()
	if err := state.ResetStream(epoch); err != nil {
		t.Fatalf("ResetStream(): %v", err)
	}
	if _, ok := state.ConfirmedProbeNonce(); ok || state.ProbeResyncRequired() {
		t.Error("stream reset retained recovery baseline")
	}
	if _, err := state.BeginProbeResync(); err == nil {
		t.Fatal("BeginProbeResync() after reset succeeded")
	} else {
		requireResyncError(t, err, ResyncCapabilityUnavailable)
	}
}

func TestStateCommandLifecycleAbandonsPendingRecovery(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00probe-buffer:b:2:1:x\x00"+
		"command-start:x\x00command-stop:0\x00prompt-ready\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	mustBeginSyncProbe(t, state)
	state.Apply(frames[2])
	abandoned, err := state.BeginProbeResync()
	if err != nil || abandoned.Value() != 1 {
		t.Fatalf("first recovery = (%d, %v)", abandoned.Value(), err)
	}
	if update := state.Apply(frames[3]); update.Kind() != UpdateCommandStarted {
		t.Errorf("command start = %d", update.Kind())
	}
	if _, ok := state.PendingProbeResyncRequestID(); ok || !state.ProbeResyncRequired() {
		t.Error("command start did not abandon recovery while retaining requirement")
	}
	state.Apply(frames[4])
	if update := state.Apply(frames[5]); update.Kind() != UpdateLifecycleSuppressed {
		t.Errorf("prompt update = %d, want suppressed", update.Kind())
	}
	if state.Foreground() != ForegroundIdle || !state.ProbeResyncRequired() || state.SuggestionsAllowed() {
		t.Error("prompt did not preserve safe recovery-required idle state")
	}
	request, err := state.BeginProbeResync()
	if err != nil || request.Value() != 2 {
		t.Errorf("new recovery = (%d, %v), want ID 2", request.Value(), err)
	}
}

func TestStateProbeResyncRequestIDsExhaustWithoutWrapping(t *testing.T) {
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	applyWire(t, state, decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	mustObserveLocalInput(t, state)
	mustBeginSyncProbe(t, state)
	applyWire(t, state, decoder, []byte("probe-buffer:b:2:1:x\x00"))
	maximum, _ := shellcontrol.NewProbeResyncRequestID(shellcontrol.MaxProbeResyncRequestID)
	state.lastProbeResyncRequestID = maximum
	state.hasLastProbeResyncID = true
	if _, err := state.BeginProbeResync(); err == nil {
		t.Fatal("BeginProbeResync() at maximum succeeded")
	} else {
		requireResyncError(t, err, ResyncRequestIDExhausted)
	}
	if !state.ProbeResyncRequired() {
		t.Error("ID exhaustion cleared recovery requirement")
	}
}

func TestStateNativeSnapshotAfterForwardedInputFailsClosed(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:native-buffer\x00prompt-ready\x00buffer:b:1:x\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	current := mustObserveLocalInput(t, state)
	update := state.Apply(frames[2])
	rejection, ok := update.SnapshotRejection()
	if !ok || rejection.Kind() != SnapshotUncorrelatedNative || rejection.CurrentGeneration() != current {
		t.Errorf("native rejection = (%v, %t)", rejection, ok)
	}
	if _, ok := state.Buffer(); ok || state.SuggestionsAllowed() {
		t.Error("uncorrelated native snapshot became authoritative")
	}
}

func TestStatePromptAndBufferWhileRunningAreRejected(t *testing.T) {
	tests := map[string]struct {
		tail string
		want LifecycleError
	}{
		"buffer": {tail: "buffer:b:1:x\x00", want: LifecycleBufferWhileCommandRunning},
		"prompt": {tail: "prompt-ready\x00", want: LifecyclePromptWhileCommandRunning},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			decoder := testDecoder()
			state := NewShellSessionState(InitialStreamEpoch())
			updates := applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
				"command-start:echo hi\x00"+tc.tail))
			lifecycle, ok := updates[3].LifecycleError()
			if !ok || lifecycle != tc.want {
				t.Errorf("LifecycleError() = (%d, %t), want %d", lifecycle, ok, tc.want)
			}
			if _, ok := state.Buffer(); ok || state.SuggestionsAllowed() {
				t.Error("running-frame rejection retained authority")
			}
		})
	}
}

func TestStateExactPreexecSurvivesLocalInputInvalidation(t *testing.T) {
	decoder := testDecoder()
	frames := decodeEvents(decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"command-start:echo hi\x00command-stop:0\x00"))
	state := NewShellSessionState(InitialStreamEpoch())
	state.Apply(frames[0])
	state.Apply(frames[1])
	mustObserveLocalInput(t, state)
	update := state.Apply(frames[2])
	details, ok := update.CommandStart()
	_, hasSource := details.Source()
	_, hasMatch := details.PreexecMatchesSnapshot()
	if !ok || !hasSource || hasMatch {
		t.Errorf("command start details = (%v, %t)", details, ok)
	}
	completed, ok := state.Apply(frames[3]).CompletedCommand()
	if !ok || !bytes.Equal(completed.Command().Bytes(), []byte("echo hi")) {
		t.Errorf("completion = (%s, %t), want exact preexec", completed, ok)
	}
}

func TestStateProbeAvailabilityAndErrors(t *testing.T) {
	state := NewShellSessionState(InitialStreamEpoch())
	if state.ProbeAvailable() {
		t.Error("ProbeAvailable() = true before handshake")
	}
	if _, err := state.BeginSyncProbe(); err == nil {
		t.Fatal("BeginSyncProbe() before handshake succeeded")
	} else {
		requireProbeError(t, err, ProbeCapabilityUnavailable)
	}
	decoder := testDecoder()
	applyWire(t, state, decoder, []byte("capability:sync-probe:0\x00prompt-ready\x00"))
	if !state.ProbeAvailable() {
		t.Error("ProbeAvailable() = false at probe prompt")
	}
	if _, err := state.BeginSyncProbe(); err != nil {
		t.Fatalf("BeginSyncProbe(): %v", err)
	}
	if state.ProbeAvailable() {
		t.Error("ProbeAvailable() = true with pending probe")
	}
	if _, err := state.BeginSyncProbe(); err == nil {
		t.Fatal("duplicate BeginSyncProbe() succeeded")
	} else {
		requireProbeError(t, err, ProbeAlreadyPending)
	}
}

func TestStateNonceAndInputGenerationExhaustionFailClosed(t *testing.T) {
	t.Run("nonce", func(t *testing.T) {
		decoder := testDecoder()
		state := NewShellSessionState(InitialStreamEpoch())
		applyWire(t, state, decoder, []byte("capability:sync-probe:18446744073709551615\x00prompt-ready\x00"))
		if _, err := state.BeginSyncProbe(); err == nil {
			t.Fatal("BeginSyncProbe() at nonce maximum succeeded")
		} else {
			requireProbeError(t, err, ProbeNonceExhausted)
		}
		if state.Synchronization() != StateDesynchronized {
			t.Error("nonce exhaustion retained editing authority")
		}
	})

	t.Run("generation", func(t *testing.T) {
		state := NewShellSessionState(InitialStreamEpoch())
		state.inputGeneration.sequence = math.MaxUint64
		if _, err := state.ObserveLocalInput(); err == nil {
			t.Fatal("ObserveLocalInput() at maximum succeeded")
		} else {
			var generationErr *InputGenerationError
			if !errors.As(err, &generationErr) {
				t.Errorf("error = %T, want InputGenerationError", err)
			}
		}
		if state.Synchronization() != StateDesynchronized || state.Foreground() != ForegroundUnknown {
			t.Error("generation exhaustion did not invalidate all authority")
		}
	})
}

func TestStateFormattingRedactsPayloads(t *testing.T) {
	secret := "Dean Pelton's secret command"
	decoder := testDecoder()
	state := NewShellSessionState(InitialStreamEpoch())
	updates := applyWire(t, state, decoder, []byte("capability:native-buffer\x00prompt-ready\x00"+
		"buffer:b:0:"+secret+"\x00command-start:"+secret+"\x00command-stop:0\x00"))
	values := map[string]any{
		"state":       state,
		"state value": *state,
		"start":       updates[3],
		"completed":   updates[4],
	}
	for name, value := range values {
		for _, format := range []string{"%v", "%+v", "%#v", "%s"} {
			if got := fmt.Sprintf(format, value); strings.Contains(got, secret) {
				t.Errorf("%s formatted with %q exposed payload: %s", name, format, got)
			}
		}
	}
}
