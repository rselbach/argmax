package config

import (
	"context"
	"testing"
	"time"

	"go.uber.org/goleak"
)

// TestWatcherStopsCleanly verifies the config watcher goroutine exits
// when its context is canceled.
func TestWatcherStopsCleanly(t *testing.T) {
	defer goleak.VerifyNone(t)
	w := NewWatcher(writeConfig(t, "[core]\nversion = 1\nmode = \"spec\"\n"), Default())
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		w.Run(ctx)
		close(done)
	}()
	time.Sleep(20 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatal("watcher goroutine did not exit after cancellation")
	}
}
