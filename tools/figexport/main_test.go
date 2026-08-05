package main

import (
	"errors"
	"testing"
	"time"

	"github.com/dop251/goja"
)

func TestRunJavaScriptInterruptsInfiniteExecution(t *testing.T) {
	started := time.Now()
	_, err := runJavaScript(goja.New(), `for (;;) {}`, 20*time.Millisecond)
	if !errors.Is(err, errJavaScriptTimeout) {
		t.Fatalf("runJavaScript() error = %v, want timeout", err)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Errorf("JavaScript interruption took %s", elapsed)
	}
}

func TestRunJavaScriptClearsLateInterrupt(t *testing.T) {
	vm := goja.New()
	if _, err := runJavaScript(vm, `1 + 1`, time.Nanosecond); err != nil && !errors.Is(err, errJavaScriptTimeout) {
		t.Fatal(err)
	}
	value, err := runJavaScript(vm, `2 + 2`, time.Second)
	if err != nil {
		t.Fatalf("runtime remained interrupted: %v", err)
	}
	if got := value.ToInteger(); got != 4 {
		t.Errorf("result = %d, want 4", got)
	}
}
