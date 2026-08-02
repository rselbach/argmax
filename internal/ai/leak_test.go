package ai

import (
	"testing"
	"time"

	"go.uber.org/goleak"

	"github.com/rselbach/argmax/internal/config"
)

// TestEngineCancelLeavesNoGoroutines verifies canceled AI requests do not
// leak their request goroutines.
func TestEngineCancelLeavesNoGoroutines(t *testing.T) {
	defer goleak.VerifyNone(t)
	e := &Engine{}
	e.Configure(config.AI{DebounceMS: 50, MinIntervalMS: 50}, &config.Provider{
		Endpoint:  "http://127.0.0.1:1", // unroutable: any dispatch fails fast
		Model:     "test",
		TimeoutMS: 100,
	})
	for range 5 {
		e.Request("git sta", func() Snapshot { return Snapshot{CWD: "/tmp"} }, func(string, int) {})
	}
	e.Cancel()
	// Any goroutine that raced past cancellation still terminates on its
	// own debounce/timeout budget.
	time.Sleep(400 * time.Millisecond)
}
