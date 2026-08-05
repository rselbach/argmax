package session

import (
	"errors"
	"os"
	"os/exec"
	"testing"
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
