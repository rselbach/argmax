package watchdog

import (
	"os"
	"os/exec"
	"path/filepath"
	"sync/atomic"
	"syscall"
	"testing"
	"time"
)

func TestForwardSignalsForcesUnresponsiveChildToExit(t *testing.T) {
	ready := filepath.Join(t.TempDir(), "ready")
	cmd := exec.Command("/bin/sh", "-c", `trap '' TERM; : > "$1"; while :; do :; done`, "sh", ready)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	signals := make(chan os.Signal, 2)
	childDone := make(chan struct{})
	forwardDone := make(chan struct{})
	var forwarded, childExited atomic.Bool
	go func() {
		forwardSignals(cmd.Process, signals, childDone, &forwarded, &childExited, 50*time.Millisecond)
		close(forwardDone)
	}()

	deadline := time.Now().Add(time.Second)
	for {
		if _, err := os.Stat(ready); err == nil {
			break
		}
		if time.Now().After(deadline) {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
			childExited.Store(true)
			close(childDone)
			<-forwardDone
			t.Fatal("child did not install signal handler")
		}
		time.Sleep(time.Millisecond)
	}
	signals <- syscall.SIGTERM
	waitDone := make(chan error, 1)
	go func() { waitDone <- cmd.Wait() }()
	select {
	case <-waitDone:
	case <-time.After(time.Second):
		_ = cmd.Process.Kill()
		<-waitDone
		childExited.Store(true)
		close(childDone)
		<-forwardDone
		t.Fatal("watchdog did not force-kill the child")
	}
	childExited.Store(true)
	close(childDone)
	select {
	case <-forwardDone:
	case <-time.After(time.Second):
		t.Fatal("signal forwarder did not stop after child exit")
	}
	if !forwarded.Load() {
		t.Error("signal was not recorded as forwarded")
	}
}
