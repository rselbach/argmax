package session

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"testing"
	"time"
)

func TestKillAndWaitReapsChild(t *testing.T) {
	cmd := exec.Command("/bin/sh", "-c", "exec sleep 30")
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	killAndWait(cmd)
	if cmd.ProcessState == nil {
		t.Fatalf("child was not reaped: process state = %v", cmd.ProcessState)
	}
	if err := cmd.Process.Signal(os.Interrupt); !errors.Is(err, os.ErrProcessDone) {
		t.Errorf("signal after reap = %v, want os.ErrProcessDone", err)
	}
}

func TestShutdownCancelsAndDrainsWorkers(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	s := &Session{
		ctx:    ctx,
		cancel: cancel,
		done:   make(chan struct{}),
	}
	started := make(chan struct{})
	finished := make(chan struct{})
	if !s.launchWorker(func() {
		close(started)
		<-s.ctx.Done()
		close(finished)
	}) {
		t.Fatal("worker did not start")
	}
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("worker did not run")
	}

	s.shutdown()
	select {
	case <-finished:
	default:
		t.Error("shutdown returned before the worker exited")
	}
	if s.launchWorker(func() {}) {
		t.Error("shutdown session accepted a new worker")
	}
}
