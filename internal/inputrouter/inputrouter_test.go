package inputrouter

import (
	"bytes"
	"errors"
	"fmt"
	"reflect"
	"strings"
	"testing"
)

var (
	ctrlSpace = []byte{0x00}
	ctrlR     = []byte{0x12}
)

func testRouter(t *testing.T) *InputRouter {
	t.Helper()
	router, err := New(ctrlSpace, ctrlR)
	if err != nil {
		t.Fatalf("New() error = %v", err)
	}
	return router
}

func routedBytes(events []RouteEvent) []byte {
	var result []byte
	for _, event := range events {
		result = append(result, event.bytes...)
	}
	return result
}

func actions(events []RouteEvent) []InputAction {
	var result []InputAction
	for _, event := range events {
		if inputAction, ok := event.Action(); ok {
			result = append(result, inputAction)
		}
	}
	return result
}

func actionKinds(events []RouteEvent) []ActionKind {
	inputActions := actions(events)
	result := make([]ActionKind, 0, len(inputActions))
	for _, inputAction := range inputActions {
		result = append(result, inputAction.Kind())
	}
	return result
}

func immediatelyForwardedBytes(events []RouteEvent) []byte {
	var result []byte
	for _, event := range events {
		if event.Forwarding() == ForwardImmediate {
			result = append(result, event.bytes...)
		}
	}
	return result
}

func assertBatchBounds(t *testing.T, batch RouteBatch) {
	t.Helper()
	if batch.ConsumedBytes() > MaxRouteBatchInputBytes {
		t.Errorf("ConsumedBytes() = %d, limit %d", batch.ConsumedBytes(), MaxRouteBatchInputBytes)
	}
	if batch.EventBytes() > MaxRouteBatchEventBytes {
		t.Errorf("EventBytes() = %d, limit %d", batch.EventBytes(), MaxRouteBatchEventBytes)
	}
	if batch.EventCount() > MaxRouteBatchEvents {
		t.Errorf("EventCount() = %d, limit %d", batch.EventCount(), MaxRouteBatchEvents)
	}
	total := 0
	for _, event := range batch.events {
		total += len(event.bytes)
	}
	if batch.EventBytes() != total {
		t.Errorf("EventBytes() = %d, summed event bytes %d", batch.EventBytes(), total)
	}
}

func routeAll(t *testing.T, router *InputRouter, input []byte) []RouteEvent {
	t.Helper()
	var events []RouteEvent
	for offset := 0; offset < len(input); {
		batch := router.Route(input[offset:])
		assertBatchBounds(t, batch)
		if batch.ConsumedBytes() <= 0 {
			t.Fatalf("Route() consumed %d bytes with %d remaining", batch.ConsumedBytes(), len(input)-offset)
		}
		offset += batch.ConsumedBytes()
		events = append(events, batch.events...)
	}
	return events
}

func routeAtEverySingleSplit(t *testing.T, input []byte) [][]RouteEvent {
	t.Helper()
	result := make([][]RouteEvent, 0, len(input)+1)
	for split := 0; split <= len(input); split++ {
		router := testRouter(t)
		events := routeAll(t, router, input[:split])
		events = append(events, routeAll(t, router, input[split:])...)
		result = append(result, events)
	}
	return result
}

func routeAtEveryPartition(t *testing.T, input []byte) [][]RouteEvent {
	t.Helper()
	boundaryCount := max(len(input)-1, 0)
	if boundaryCount >= 63 {
		t.Fatalf("input has %d partition boundaries", boundaryCount)
	}
	result := make([][]RouteEvent, 0, 1<<boundaryCount)
	for partition := uint64(0); partition < uint64(1)<<boundaryCount; partition++ {
		router := testRouter(t)
		var events []RouteEvent
		start := 0
		for boundary := 1; boundary < len(input); boundary++ {
			if partition&(uint64(1)<<(boundary-1)) != 0 {
				events = append(events, routeAll(t, router, input[start:boundary])...)
				start = boundary
			}
		}
		events = append(events, routeAll(t, router, input[start:])...)
		if router.PendingLen() != 0 {
			t.Fatalf("partition %d PendingLen() = %d, want 0", partition, router.PendingLen())
		}
		result = append(result, events)
	}
	return result
}

func TestLiveBindingChangeWaitsForRetainedSequencePrefix(t *testing.T) {
	router, err := New([]byte("\x1b[1;2P"), ctrlR)
	if err != nil {
		t.Fatal(err)
	}
	if events := routeAll(t, router, []byte("\x1b[1;")); len(events) != 0 {
		t.Fatalf("partial sequence emitted %#v", events)
	}
	changed, err := router.Reconfigure(ctrlSpace, []byte("\x1b[Z"))
	if err != nil || changed {
		t.Fatalf("Reconfigure() = (%t, %v), want (false, nil)", changed, err)
	}

	completed := routeAll(t, router, []byte("2P"))
	if got := actionKinds(completed); !reflect.DeepEqual(got, []ActionKind{ActionToggleMode}) {
		t.Errorf("completed actions = %v, want ToggleMode", got)
	}
	changed, err = router.Reconfigure(ctrlSpace, []byte("\x1b[Z"))
	if err != nil || !changed {
		t.Fatalf("Reconfigure() = (%t, %v), want (true, nil)", changed, err)
	}
	if got := actionKinds(routeAll(t, router, []byte("\x1b[Z"))); !reflect.DeepEqual(got, []ActionKind{ActionToggleMenu}) {
		t.Errorf("reconfigured actions = %v, want ToggleMenu", got)
	}
}

func TestPrintableUTF8IsCompleteAndBytePreservingAtEverySplit(t *testing.T) {
	input := []byte("Aé界🦀")
	wantActions := []InputAction{
		printable('A'), printable('é'), printable('界'), printable('🦀'),
	}
	for split, events := range routeAtEverySingleSplit(t, input) {
		if !bytes.Equal(routedBytes(events), input) {
			t.Errorf("split %d routed %q, want %q", split, routedBytes(events), input)
		}
		if !reflect.DeepEqual(actions(events), wantActions) {
			t.Errorf("split %d actions = %v, want %v", split, actions(events), wantActions)
		}
		for _, event := range events {
			if event.Forwarding() != ForwardImmediate {
				t.Errorf("split %d forwarding = %s, want Immediate", split, event.Forwarding())
			}
		}
	}
}

func TestMalformedAndIncompleteUTF8ForwardsAndDesynchronizes(t *testing.T) {
	router := testRouter(t)
	events := routeAll(t, router, []byte{0xe2, '(', 0xa1, 0xf0, 0x9f})
	if router.PendingLen() != 2 {
		t.Fatalf("PendingLen() = %d, want 2", router.PendingLen())
	}
	if batch := router.FlushPending(); batch.EventCount() != 0 {
		t.Fatalf("FlushPending() = %s, want no events", batch)
	}
	if router.PendingLen() != 2 {
		t.Fatalf("PendingLen() after flush = %d, want 2", router.PendingLen())
	}
	events = append(events, router.Finish().events...)

	wantBytes := []byte{0xe2, '(', 0xa1, 0xf0, 0x9f}
	if !bytes.Equal(routedBytes(events), wantBytes) {
		t.Errorf("routed bytes = %v, want %v", routedBytes(events), wantBytes)
	}
	wantKinds := []ActionKind{
		ActionDesynchronize, ActionPrintable, ActionDesynchronize, ActionDesynchronize,
	}
	if !reflect.DeepEqual(actionKinds(events), wantKinds) {
		t.Errorf("actions = %v, want %v", actionKinds(events), wantKinds)
	}
	if router.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d after finish, want 0", router.PendingLen())
	}
}

func TestFixedEscapeSequencesDecodeAtEveryBoundary(t *testing.T) {
	tests := map[string]struct {
		sequence   []byte
		action     ActionKind
		forwarding Forwarding
	}{
		"CSI up":       {[]byte("\x1b[A"), ActionArrowUp, ForwardOnFallback},
		"CSI down":     {[]byte("\x1b[B"), ActionArrowDown, ForwardOnFallback},
		"CSI right":    {[]byte("\x1b[C"), ActionArrowRight, ForwardOnFallback},
		"CSI left":     {[]byte("\x1b[D"), ActionArrowLeft, ForwardOnFallback},
		"SS3 up":       {[]byte("\x1bOA"), ActionArrowUp, ForwardOnFallback},
		"SS3 down":     {[]byte("\x1bOB"), ActionArrowDown, ForwardOnFallback},
		"SS3 right":    {[]byte("\x1bOC"), ActionArrowRight, ForwardOnFallback},
		"SS3 left":     {[]byte("\x1bOD"), ActionArrowLeft, ForwardOnFallback},
		"shift tab":    {[]byte("\x1b[Z"), ActionShiftTab, ForwardOnFallback},
		"delete":       {[]byte("\x1b[3~"), ActionDelete, ForwardImmediate},
		"CSI home":     {[]byte("\x1b[H"), ActionHome, ForwardImmediate},
		"SS3 home":     {[]byte("\x1bOH"), ActionHome, ForwardImmediate},
		"tilde home 1": {[]byte("\x1b[1~"), ActionHome, ForwardImmediate},
		"tilde home 7": {[]byte("\x1b[7~"), ActionHome, ForwardImmediate},
		"CSI end":      {[]byte("\x1b[F"), ActionEnd, ForwardImmediate},
		"SS3 end":      {[]byte("\x1bOF"), ActionEnd, ForwardImmediate},
		"tilde end 4":  {[]byte("\x1b[4~"), ActionEnd, ForwardImmediate},
		"tilde end 8":  {[]byte("\x1b[8~"), ActionEnd, ForwardImmediate},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			for split, events := range routeAtEverySingleSplit(t, tc.sequence) {
				if !bytes.Equal(routedBytes(events), tc.sequence) {
					t.Errorf("split %d bytes = %v, want %v", split, routedBytes(events), tc.sequence)
				}
				if got := actionKinds(events); !reflect.DeepEqual(got, []ActionKind{tc.action}) {
					t.Errorf("split %d actions = %v, want %s", split, got, tc.action)
				}
				if len(events) != 1 || events[0].Forwarding() != tc.forwarding {
					t.Errorf("split %d events = %#v, want forwarding %s", split, events, tc.forwarding)
				}
			}
		})
	}
}

func TestConfiguredSequencesTakePrecedenceAndKeepFallbackBytes(t *testing.T) {
	router, err := New([]byte("\x1b[Z"), ctrlSpace)
	if err != nil {
		t.Fatal(err)
	}
	events := routeAll(t, router, []byte("\x1b[Z\x00"))
	if !bytes.Equal(routedBytes(events), []byte("\x1b[Z\x00")) {
		t.Errorf("routed bytes = %v", routedBytes(events))
	}
	want := []ActionKind{ActionToggleMode, ActionToggleMenu}
	if !reflect.DeepEqual(actionKinds(events), want) {
		t.Errorf("actions = %v, want %v", actionKinds(events), want)
	}
	for _, event := range events {
		if event.Forwarding() != ForwardOnFallback {
			t.Errorf("forwarding = %s, want OnFallback", event.Forwarding())
		}
	}
}

func TestStandaloneEscapeRequiresExplicitFlush(t *testing.T) {
	router := testRouter(t)
	if events := routeAll(t, router, []byte{escape}); len(events) != 0 {
		t.Fatalf("Route(Escape) emitted %#v", events)
	}
	if router.PendingLen() != 1 {
		t.Fatalf("PendingLen() = %d, want 1", router.PendingLen())
	}

	batch := router.FlushPending()
	if !bytes.Equal(routedBytes(batch.events), []byte{escape}) ||
		!reflect.DeepEqual(actionKinds(batch.events), []ActionKind{ActionEscape}) ||
		batch.events[0].Forwarding() != ForwardImmediate {
		t.Errorf("FlushPending() = %s, want immediate Escape", batch)
	}
	if router.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d, want 0", router.PendingLen())
	}
}

func TestUnknownAndIncompleteEscapeSequencesAreNeverSwallowed(t *testing.T) {
	for name, input := range map[string][]byte{
		"CSI": []byte("\x1b[9~"),
		"OSC": []byte("\x1b]unknown\x07"),
	} {
		t.Run(name, func(t *testing.T) {
			for split := 0; split <= len(input); split++ {
				router := testRouter(t)
				events := routeAll(t, router, input[:split])
				events = append(events, routeAll(t, router, input[split:])...)
				events = append(events, router.FlushPending().events...)
				if !bytes.Equal(routedBytes(events), input) {
					t.Errorf("split %d bytes = %v, want %v", split, routedBytes(events), input)
				}
				found := false
				for _, kind := range actionKinds(events) {
					found = found || kind == ActionDesynchronize
				}
				if !found {
					t.Errorf("split %d actions = %v, want Desynchronize", split, actionKinds(events))
				}
			}
		})
	}

	router := testRouter(t)
	if events := routeAll(t, router, []byte("\x1b[")); len(events) != 0 {
		t.Fatalf("partial CSI emitted %#v", events)
	}
	if router.FlushPending().EventCount() != 0 || router.PendingLen() != 2 {
		t.Fatalf("flush changed partial CSI: pending %d", router.PendingLen())
	}
	finish := router.Finish()
	if !bytes.Equal(routedBytes(finish.events), []byte("\x1b[")) ||
		!reflect.DeepEqual(actionKinds(finish.events), []ActionKind{ActionDesynchronize}) {
		t.Errorf("Finish() = %s, want desynchronized partial CSI", finish)
	}
}

func TestGenericTerminalControlsAreOneDesynchronizationAtEveryPartition(t *testing.T) {
	tests := map[string][]byte{
		"CSI modified key":         []byte("\x1b[1;5D"),
		"CSI private":              []byte("\x1b[?25l"),
		"CSI intermediate":         []byte("\x1b[1 q"),
		"CSI malformed transition": []byte("\x1b[1 ;5D"),
		"CSI embedded control":     []byte("\x1b[1\x013D"),
		"SS3 function":             []byte("\x1bOP"),
		"ESC intermediate":         []byte("\x1b(0"),
		"OSC bell":                 []byte("\x1b]0;Troy\x07"),
		"OSC ST":                   []byte("\x1b]Troy\x1b\\"),
		"DCS ST":                   []byte("\x1bPqTroy\x07\x1b\\"),
		"SOS ST":                   []byte("\x1bXTroy\x1b\\"),
		"PM ST":                    []byte("\x1b^Troy\x1b\\"),
		"APC ST":                   []byte("\x1b_Troy\x1b\\"),
	}
	for name, input := range tests {
		t.Run(name, func(t *testing.T) {
			for partition, events := range routeAtEveryPartition(t, input) {
				if !bytes.Equal(routedBytes(events), input) {
					t.Fatalf("partition %d bytes = %v, want %v", partition, routedBytes(events), input)
				}
				if !reflect.DeepEqual(actionKinds(events), []ActionKind{ActionDesynchronize}) {
					t.Fatalf("partition %d actions = %v", partition, actionKinds(events))
				}
				if events[0].Forwarding() != ForwardImmediate {
					t.Errorf("partition %d forwarding = %s", partition, events[0].Forwarding())
				}
			}
		})
	}
}

func TestMalformedEscapePayloadIsHeldFailClosedUntilEOF(t *testing.T) {
	input := []byte("\x1b\xc3\xa9")
	router := testRouter(t)
	if events := routeAll(t, router, input); len(events) != 0 {
		t.Fatalf("Route() emitted %#v", events)
	}
	if router.PendingLen() != len(input) {
		t.Fatalf("PendingLen() = %d, want %d", router.PendingLen(), len(input))
	}
	if router.FlushPending().EventCount() != 0 || router.PendingLen() != len(input) {
		t.Fatal("FlushPending() resolved malformed Escape payload")
	}
	batch := router.Finish()
	assertBatchBounds(t, batch)
	if !bytes.Equal(routedBytes(batch.events), input) ||
		!reflect.DeepEqual(actionKinds(batch.events), []ActionKind{ActionDesynchronize}) {
		t.Errorf("Finish() = %s, want exact desynchronization", batch)
	}
	if router.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d after finish", router.PendingLen())
	}
}

func TestTimeoutNeverTurnsPartialTerminalControlsIntoSafeBoundaries(t *testing.T) {
	tests := map[string][]byte{
		"CSI": []byte("\x1b[1;"),
		"OSC": []byte("\x1b]Troy"),
		"DCS": []byte("\x1bPqTroy"),
		"SOS": []byte("\x1bXTroy"),
		"PM":  []byte("\x1b^Troy"),
		"APC": []byte("\x1b_Troy"),
	}
	for name, input := range tests {
		t.Run(name, func(t *testing.T) {
			router := testRouter(t)
			if events := routeAll(t, router, input); len(events) != 0 {
				t.Fatalf("Route() emitted %#v", events)
			}
			if router.PendingLen() != len(input) {
				t.Fatalf("PendingLen() = %d, want %d", router.PendingLen(), len(input))
			}
			if router.FlushPending().EventCount() != 0 || router.PendingLen() != len(input) {
				t.Fatal("FlushPending() resolved partial control")
			}
			events := router.Finish().events
			if !bytes.Equal(routedBytes(events), input) ||
				!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionDesynchronize}) {
				t.Errorf("Finish events = %#v", events)
			}
			if router.PendingLen() != 0 {
				t.Errorf("PendingLen() = %d after finish", router.PendingLen())
			}
		})
	}
}

func TestGenericControlRestartNeverExposesPrintableTail(t *testing.T) {
	input := []byte("\x1b[12\x1b[A")
	for split := 0; split <= len(input); split++ {
		router := testRouter(t)
		events := routeAll(t, router, input[:split])
		events = append(events, routeAll(t, router, input[split:])...)
		if !bytes.Equal(routedBytes(events), input) {
			t.Errorf("split %d bytes = %v, want %v", split, routedBytes(events), input)
		}
		want := []ActionKind{ActionDesynchronize, ActionArrowUp}
		if !reflect.DeepEqual(actionKinds(events), want) {
			t.Errorf("split %d actions = %v, want %v", split, actionKinds(events), want)
		}
		for _, kind := range actionKinds(events) {
			if kind == ActionPrintable {
				t.Errorf("split %d exposed printable input", split)
			}
		}
	}
}

func TestOversizedAndIncompleteControlsAreBoundedAndFailClosed(t *testing.T) {
	complete := append([]byte("\x1b]"), bytes.Repeat([]byte{'x'}, MaxRetainedPrefixBytes*4)...)
	complete = append(complete, []byte("\x1b\\")...)
	completeRouter := testRouter(t)
	completeEvents := routeAll(t, completeRouter, complete)
	if !bytes.Equal(routedBytes(completeEvents), complete) {
		t.Fatalf("complete bytes differ")
	}
	if len(completeEvents) <= 1 {
		t.Fatalf("complete emitted %d events, want multiple bounded chunks", len(completeEvents))
	}
	for _, event := range completeEvents {
		inputAction, ok := event.Action()
		if event.Len() > MaxRetainedPrefixBytes || event.Forwarding() != ForwardImmediate ||
			!ok || inputAction.Kind() != ActionDesynchronize {
			t.Errorf("complete event = %s, want bounded immediate desynchronization", event)
		}
	}
	if completeRouter.PendingLen() != 0 {
		t.Errorf("complete PendingLen() = %d", completeRouter.PendingLen())
	}

	incomplete := append([]byte("\x1bP"), bytes.Repeat([]byte{'y'}, MaxRetainedPrefixBytes*4)...)
	incompleteRouter := testRouter(t)
	incompleteEvents := routeAll(t, incompleteRouter, incomplete)
	finish := incompleteRouter.Finish()
	assertBatchBounds(t, finish)
	incompleteEvents = append(incompleteEvents, finish.events...)
	if !bytes.Equal(routedBytes(incompleteEvents), incomplete) {
		t.Fatalf("incomplete bytes differ")
	}
	for _, kind := range actionKinds(incompleteEvents) {
		if kind != ActionDesynchronize {
			t.Errorf("incomplete action = %s, want Desynchronize", kind)
		}
	}
	if incompleteRouter.PendingLen() != 0 || incompleteRouter.Finish().EventCount() != 0 {
		t.Errorf("incomplete router did not finish idempotently")
	}
}

func TestFullControlPrefixPlusOneBatchHitsEventBound(t *testing.T) {
	prefix := append([]byte("\x1b]"), bytes.Repeat(
		[]byte{'x'}, MaxRetainedPrefixBytes-len([]byte("\x1b]")),
	)...)
	router := testRouter(t)
	first := router.Route(prefix)
	assertBatchBounds(t, first)
	if first.ConsumedBytes() != MaxRetainedPrefixBytes || first.EventBytes() != 0 ||
		router.PendingLen() != MaxRetainedPrefixBytes {
		t.Fatalf("first = %s, pending %d", first, router.PendingLen())
	}

	input := append(bytes.Repeat([]byte{'x'}, MaxRouteBatchInputBytes-1), 0x07)
	batch := router.Route(input)
	assertBatchBounds(t, batch)
	if batch.ConsumedBytes() != MaxRouteBatchInputBytes ||
		batch.EventBytes() != MaxRouteBatchEventBytes {
		t.Errorf("batch = %s, want consumed %d event bytes %d", batch, MaxRouteBatchInputBytes, MaxRouteBatchEventBytes)
	}
	want := append(bytes.Clone(prefix), input...)
	if !bytes.Equal(routedBytes(batch.events), want) {
		t.Errorf("routed bytes differ")
	}
	for _, kind := range actionKinds(batch.events) {
		if kind != ActionDesynchronize {
			t.Errorf("action = %s, want Desynchronize", kind)
		}
	}
	if router.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d, want 0", router.PendingLen())
	}
}

func TestPasteEndMarkerOutsidePasteForwardsAndDesynchronizes(t *testing.T) {
	for split, events := range routeAtEverySingleSplit(t, bracketedPasteEnd) {
		if !bytes.Equal(routedBytes(events), bracketedPasteEnd) {
			t.Errorf("split %d bytes = %v", split, routedBytes(events))
		}
		foundDesync := false
		for _, kind := range actionKinds(events) {
			foundDesync = foundDesync || kind == ActionDesynchronize
			if kind == ActionPasteEnd {
				t.Errorf("split %d emitted PasteEnd outside paste", split)
			}
		}
		if !foundDesync {
			t.Errorf("split %d did not desynchronize", split)
		}
		for _, event := range events {
			if event.Forwarding() != ForwardImmediate {
				t.Errorf("split %d forwarding = %s", split, event.Forwarding())
			}
		}
	}
}

func TestTabHasFallbackBytesAndEnterUsesShellBuffer(t *testing.T) {
	router := testRouter(t)
	events := routeAll(t, router, []byte("\t\r\n"))
	if !bytes.Equal(routedBytes(events), []byte("\t\r\n")) || len(events) != 3 {
		t.Fatalf("events = %#v", events)
	}
	firstAction, firstOK := events[0].Action()
	secondAction, secondOK := events[1].Action()
	_, thirdOK := events[2].Action()
	if !firstOK || firstAction.Kind() != ActionTab ||
		events[0].Forwarding() != ForwardOnFallback ||
		!bytes.Equal(events[0].bytes, []byte("\t")) {
		t.Errorf("Tab event = %s", events[0])
	}
	if !secondOK || secondAction.Kind() != ActionEnter ||
		events[1].Forwarding() != ForwardImmediate {
		t.Errorf("Enter event = %s", events[1])
	}
	if thirdOK || events[2].Forwarding() != ForwardSuppress {
		t.Errorf("LF event = %s", events[2])
	}
	if !bytes.Equal(immediatelyForwardedBytes(events), []byte("\r")) {
		t.Errorf("immediately forwarded = %q, want CR", immediatelyForwardedBytes(events))
	}
}

func TestCRLFIsOneEnterAcrossChunksAndFlush(t *testing.T) {
	crlfRouter := testRouter(t)
	events := routeAll(t, crlfRouter, []byte("\r"))
	events = append(events, routeAll(t, crlfRouter, []byte("\n"))...)
	if !bytes.Equal(routedBytes(events), []byte("\r\n")) ||
		!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionEnter}) ||
		events[1].Forwarding() != ForwardSuppress ||
		!bytes.Equal(immediatelyForwardedBytes(events), []byte("\r")) {
		t.Errorf("chunked CRLF events = %#v", events)
	}

	desynchronizedRouter := testRouter(t)
	events = routeAll(t, desynchronizedRouter, []byte("\x1b[\r\n"))
	if len(events) != 0 {
		t.Fatalf("partial CSI with CRLF emitted %#v", events)
	}
	events = append(events, desynchronizedRouter.Finish().events...)
	if !bytes.Equal(routedBytes(events), []byte("\x1b[\r\n")) ||
		!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionDesynchronize}) ||
		!bytes.Equal(immediatelyForwardedBytes(events), []byte("\x1b[\r\n")) {
		t.Errorf("desynchronized events = %#v", events)
	}

	flushedRouter := testRouter(t)
	events = routeAll(t, flushedRouter, []byte("\r"))
	events = append(events, flushedRouter.FlushPending().events...)
	events = append(events, routeAll(t, flushedRouter, []byte("\n"))...)
	if !reflect.DeepEqual(actionKinds(events), []ActionKind{ActionEnter}) ||
		events[len(events)-1].Forwarding() != ForwardSuppress ||
		!bytes.Equal(immediatelyForwardedBytes(events), []byte("\r")) {
		t.Errorf("flushed CRLF events = %#v", events)
	}
}

func TestFixedControlsAreForwardedWithCoherentActions(t *testing.T) {
	input := []byte{0x01, 0x03, 0x05, 0x08, 0x0c, 0x15, 0x17, 0x7f}
	events := routeAll(t, testRouter(t), input)
	wantKinds := []ActionKind{
		ActionCtrlA, ActionCtrlC, ActionCtrlE, ActionBackspace,
		ActionCtrlL, ActionCtrlU, ActionCtrlW, ActionBackspace,
	}
	if !bytes.Equal(routedBytes(events), input) ||
		!reflect.DeepEqual(actionKinds(events), wantKinds) {
		t.Errorf("events = %#v, want actions %v", events, wantKinds)
	}
	for _, event := range events {
		if event.Forwarding() != ForwardImmediate {
			t.Errorf("event = %s, want Immediate", event)
		}
	}
}

func TestBracketedPasteIsVerbatimAndNonsemanticAtEverySplit(t *testing.T) {
	payload := []byte("A\x03\xff\t\r\n\x1b[20xB")
	input := append(bytes.Clone(bracketedPasteStart), payload...)
	input = append(input, bracketedPasteEnd...)

	for split := 0; split <= len(input); split++ {
		router := testRouter(t)
		events := routeAll(t, router, input[:split])
		events = append(events, routeAll(t, router, input[split:])...)
		if !bytes.Equal(routedBytes(events), input) {
			t.Errorf("split %d routed bytes differ", split)
		}
		kinds := actionKinds(events)
		if len(kinds) < 2 || kinds[0] != ActionPasteStart ||
			kinds[len(kinds)-1] != ActionPasteEnd {
			t.Errorf("split %d actions = %v", split, kinds)
		}
		for _, kind := range kinds {
			if kind != ActionPasteStart && kind != ActionPasteData && kind != ActionPasteEnd {
				t.Errorf("split %d semantic paste action = %s", split, kind)
			}
		}
		if router.IsBracketedPaste() || router.PendingLen() != 0 {
			t.Errorf("split %d left paste=%t pending=%d", split, router.IsBracketedPaste(), router.PendingLen())
		}
	}
}

func TestPasteMarkersDecodeWithOneByteChunks(t *testing.T) {
	input := []byte("\x1b[200~Troy\x1b[201~")
	router := testRouter(t)
	var events []RouteEvent
	for _, char := range input {
		events = append(events, routeAll(t, router, []byte{char})...)
		if router.PendingLen() > MaxRetainedPrefixBytes {
			t.Fatalf("PendingLen() = %d", router.PendingLen())
		}
	}
	kinds := actionKinds(events)
	if !bytes.Equal(routedBytes(events), input) || len(kinds) < 2 ||
		kinds[0] != ActionPasteStart || kinds[len(kinds)-1] != ActionPasteEnd {
		t.Errorf("events = %#v", events)
	}
}

func TestPastePayloadStreamsWithOnlyEndPrefixRetained(t *testing.T) {
	router := testRouter(t)
	start := routeAll(t, router, bracketedPasteStart)
	if !reflect.DeepEqual(actionKinds(start), []ActionKind{ActionPasteStart}) {
		t.Fatalf("start actions = %v", actionKinds(start))
	}

	payload := bytes.Repeat([]byte{'x'}, 256*1024)
	events := routeAll(t, router, payload)
	if !bytes.Equal(routedBytes(events), payload) {
		t.Fatal("payload bytes differ")
	}
	for _, kind := range actionKinds(events) {
		if kind != ActionPasteData {
			t.Errorf("payload action = %s", kind)
		}
	}
	if router.PendingLen() != 0 {
		t.Errorf("PendingLen() = %d after plain payload", router.PendingLen())
	}

	events = routeAll(t, router, []byte("tail\x1b"))
	if !bytes.Equal(routedBytes(events), []byte("tail")) || router.PendingLen() != 1 {
		t.Errorf("tail events = %#v, pending %d", events, router.PendingLen())
	}
	if router.FlushPending().EventCount() != 0 || router.PendingLen() != 1 {
		t.Fatal("FlushPending() resolved paste end prefix")
	}

	events = routeAll(t, router, []byte("x"))
	events = append(events, routeAll(t, router, bracketedPasteEnd)...)
	if !bytes.Equal(routedBytes(events), []byte("\x1bx\x1b[201~")) {
		t.Errorf("final bytes = %v", routedBytes(events))
	}
	kinds := actionKinds(events)
	if kinds[len(kinds)-1] != ActionPasteEnd || router.IsBracketedPaste() {
		t.Errorf("final actions = %v, paste=%t", kinds, router.IsBracketedPaste())
	}
}

func TestFinishDrainsIncompleteNormalInputAndIsIdempotent(t *testing.T) {
	escapeRouter := testRouter(t)
	if events := routeAll(t, escapeRouter, []byte("\x1b[")); len(events) != 0 {
		t.Fatalf("partial CSI emitted %#v", events)
	}
	batch := escapeRouter.Finish()
	assertBatchBounds(t, batch)
	if batch.ConsumedBytes() != 0 || !bytes.Equal(routedBytes(batch.events), []byte("\x1b[")) ||
		!reflect.DeepEqual(actionKinds(batch.events), []ActionKind{ActionDesynchronize}) ||
		escapeRouter.PendingLen() != 0 || escapeRouter.Finish().EventCount() != 0 {
		t.Errorf("Finish partial CSI = %s", batch)
	}

	standaloneEscapeRouter := testRouter(t)
	if events := routeAll(t, standaloneEscapeRouter, []byte("\x1b")); len(events) != 0 {
		t.Fatalf("Escape emitted %#v", events)
	}
	events := standaloneEscapeRouter.Finish().events
	if !bytes.Equal(routedBytes(events), []byte("\x1b")) ||
		!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionEscape}) {
		t.Errorf("Finish Escape events = %#v", events)
	}

	utf8Router := testRouter(t)
	if events := routeAll(t, utf8Router, []byte{0xf0, 0x9f}); len(events) != 0 {
		t.Fatalf("partial UTF-8 emitted %#v", events)
	}
	events = utf8Router.Finish().events
	if !bytes.Equal(routedBytes(events), []byte{0xf0, 0x9f}) ||
		!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionDesynchronize}) ||
		utf8Router.PendingLen() != 0 {
		t.Errorf("Finish UTF-8 events = %#v", events)
	}
}

func TestFinishDesynchronizesTruncatedPasteWithoutSyntheticInput(t *testing.T) {
	prefixedRouter := testRouter(t)
	priorEvents := routeAll(t, prefixedRouter, bracketedPasteStart)
	priorEvents = append(priorEvents, routeAll(t, prefixedRouter, []byte("Greendale\x1b[20"))...)
	if !prefixedRouter.IsBracketedPaste() || prefixedRouter.PendingLen() != 4 {
		t.Fatalf("paste=%t pending=%d", prefixedRouter.IsBracketedPaste(), prefixedRouter.PendingLen())
	}
	batch := prefixedRouter.Finish()
	assertBatchBounds(t, batch)
	if !bytes.Equal(routedBytes(batch.events), []byte("\x1b[20")) ||
		!reflect.DeepEqual(actionKinds(batch.events), []ActionKind{ActionDesynchronize}) ||
		batch.events[0].Forwarding() != ForwardImmediate || prefixedRouter.IsBracketedPaste() ||
		prefixedRouter.PendingLen() != 0 {
		t.Errorf("Finish prefixed paste = %s", batch)
	}
	if !bytes.Equal(routedBytes(priorEvents), []byte("\x1b[200~Greendale")) {
		t.Errorf("prior bytes = %v", routedBytes(priorEvents))
	}

	emptyRouter := testRouter(t)
	start := routeAll(t, emptyRouter, bracketedPasteStart)
	if !reflect.DeepEqual(actionKinds(start), []ActionKind{ActionPasteStart}) {
		t.Fatalf("start actions = %v", actionKinds(start))
	}
	batch = emptyRouter.Finish()
	inputAction, ok := batch.events[0].Action()
	if batch.EventBytes() != 0 || batch.EventCount() != 1 || batch.events[0].Len() != 0 ||
		batch.events[0].Forwarding() != ForwardSuppress || !ok ||
		inputAction.Kind() != ActionDesynchronize || emptyRouter.IsBracketedPaste() ||
		emptyRouter.Finish().EventCount() != 0 {
		t.Errorf("Finish empty paste = %s", batch)
	}
}

func TestHugeNormalAndPasteStreamsStayWithinBatchLimits(t *testing.T) {
	normalInput := bytes.Repeat([]byte{'a'}, MaxRouteBatchInputBytes*4097+17)
	normalRouter := testRouter(t)
	for offset := 0; offset < len(normalInput); {
		batch := normalRouter.Route(normalInput[offset:])
		assertBatchBounds(t, batch)
		wantConsumed := min(len(normalInput)-offset, MaxRouteBatchInputBytes)
		if batch.ConsumedBytes() != wantConsumed ||
			!bytes.Equal(routedBytes(batch.events), normalInput[offset:offset+wantConsumed]) {
			t.Fatalf("normal offset %d batch = %s", offset, batch)
		}
		for _, event := range batch.events {
			inputAction, ok := event.Action()
			character, printableOK := inputAction.Character()
			if event.Forwarding() != ForwardImmediate || !ok || !printableOK || character != 'a' {
				t.Fatalf("normal event = %s", event)
			}
		}
		offset += batch.ConsumedBytes()
	}

	pasteInput := bytes.Repeat([]byte{'x'}, MaxRouteBatchInputBytes*4097+17)
	pasteRouter := testRouter(t)
	if got := actionKinds(routeAll(t, pasteRouter, bracketedPasteStart)); !reflect.DeepEqual(got, []ActionKind{ActionPasteStart}) {
		t.Fatalf("paste start actions = %v", got)
	}
	for offset := 0; offset < len(pasteInput); {
		batch := pasteRouter.Route(pasteInput[offset:])
		assertBatchBounds(t, batch)
		wantConsumed := min(len(pasteInput)-offset, MaxRouteBatchInputBytes)
		if batch.ConsumedBytes() != wantConsumed ||
			!bytes.Equal(routedBytes(batch.events), pasteInput[offset:offset+wantConsumed]) {
			t.Fatalf("paste offset %d batch = %s", offset, batch)
		}
		for _, kind := range actionKinds(batch.events) {
			if kind != ActionPasteData {
				t.Fatalf("paste action = %s", kind)
			}
		}
		if pasteRouter.PendingLen() > MaxRetainedPrefixBytes {
			t.Fatalf("paste PendingLen() = %d", pasteRouter.PendingLen())
		}
		offset += batch.ConsumedBytes()
	}
	if got := actionKinds(routeAll(t, pasteRouter, bracketedPasteEnd)); !reflect.DeepEqual(got, []ActionKind{ActionPasteEnd}) || pasteRouter.IsBracketedPaste() {
		t.Errorf("paste end actions = %v, paste=%t", got, pasteRouter.IsBracketedPaste())
	}
}

func TestDebugDiagnosticsRedactInputAndBindings(t *testing.T) {
	inputAction := printable('T')
	actionDebug := fmt.Sprintf("%#v", inputAction)
	if actionDebug != "Printable" || strings.Contains(actionDebug, "T") {
		t.Errorf("action debug = %q", actionDebug)
	}

	event := newRouteEvent(
		[]byte("Troy"), ForwardImmediate, printable('T'), true,
	)
	eventDebug := fmt.Sprintf("%#v", event)
	wantEvent := "RouteEvent { byte_count: 4, forwarding: Immediate, action: Some(Printable) }"
	if eventDebug != wantEvent || strings.Contains(eventDebug, "Troy") ||
		strings.Contains(eventDebug, "'T'") {
		t.Errorf("event debug = %q, want %q", eventDebug, wantEvent)
	}

	batchDebug := fmt.Sprintf("%#v", newRouteBatch(4, []RouteEvent{event}))
	if !strings.Contains(batchDebug, "event_count: 1") ||
		!strings.Contains(batchDebug, "action: Some(Printable)") ||
		strings.Contains(batchDebug, "Troy") || strings.Contains(batchDebug, "'T'") {
		t.Errorf("batch debug = %q", batchDebug)
	}

	router, err := New([]byte("Greendale"), []byte("Community"))
	if err != nil {
		t.Fatal(err)
	}
	routerDebug := fmt.Sprintf("%#v", router)
	if !strings.Contains(routerDebug, "toggle_mode_bytes: 9") ||
		!strings.Contains(routerDebug, "toggle_menu_bytes: 9") ||
		strings.Contains(routerDebug, "Greendale") || strings.Contains(routerDebug, "Community") {
		t.Errorf("router debug = %q", routerDebug)
	}

	copied := event.Bytes()
	copied[0] = 'X'
	if bytes.Equal(copied, event.Bytes()) {
		t.Error("mutating Bytes() result changed RouteEvent")
	}
}

func TestRouterConfigurationAndPendingStateAreHardBounded(t *testing.T) {
	_, err := New(nil, ctrlR)
	var routerError *InputRouterError
	if !errors.As(err, &routerError) || routerError.Kind() != ErrorEmptySequence ||
		routerError.Action() != ConfiguredToggleMode {
		t.Fatalf("New(empty) error = %#v", err)
	}

	oversized := bytes.Repeat([]byte{'x'}, MaxConfiguredSequenceBytes+1)
	_, err = New(ctrlSpace, oversized)
	if !errors.As(err, &routerError) || routerError.Kind() != ErrorSequenceTooLong ||
		routerError.Action() != ConfiguredToggleMenu ||
		routerError.ObservedBytes() != MaxConfiguredSequenceBytes+1 ||
		routerError.Limit() != MaxConfiguredSequenceBytes {
		t.Fatalf("New(oversized) error = %#v", err)
	}
	if strings.Contains(err.Error(), strings.Repeat("x", 2)) {
		t.Errorf("error leaked binding contents: %v", err)
	}

	longest := bytes.Repeat([]byte{'x'}, MaxConfiguredSequenceBytes)
	router, err := New(longest, ctrlR)
	if err != nil {
		t.Fatal(err)
	}
	prefix := longest[:len(longest)-1]
	if events := routeAll(t, router, prefix); len(events) != 0 {
		t.Fatalf("long prefix emitted %#v", events)
	}
	if router.PendingLen() != MaxRetainedPrefixBytes-1 {
		t.Fatalf("PendingLen() = %d", router.PendingLen())
	}
	if router.FlushPending().EventCount() != 0 ||
		router.PendingLen() != MaxRetainedPrefixBytes-1 {
		t.Fatal("FlushPending() resolved configured prefix")
	}
	events := router.Finish().events
	if !bytes.Equal(routedBytes(events), prefix) ||
		!reflect.DeepEqual(actionKinds(events), []ActionKind{ActionDesynchronize}) ||
		router.PendingLen() != 0 {
		t.Errorf("Finish configured prefix events = %#v", events)
	}
}
