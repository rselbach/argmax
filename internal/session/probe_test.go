package session

import (
	"testing"
	"time"
)

func TestProbeRunsInRequestedWorkingDirectory(t *testing.T) {
	cwd := t.TempDir()
	if got := probe(cwd, time.Second, "/bin/sh", "-c", "pwd"); got != cwd {
		t.Errorf("probe working directory = %q, want %q", got, cwd)
	}
}
