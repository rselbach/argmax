package generators

import (
	"testing"
	"time"
)

func TestRunDiscardsPartialOutputOnFailure(t *testing.T) {
	if got := run(t.TempDir(), time.Second, "sh", "-c", "printf partial; exit 1"); got != "" {
		t.Errorf("failed probe returned partial output %q", got)
	}
}

func TestRunDiscardsPartialOutputOnTimeout(t *testing.T) {
	if got := run(t.TempDir(), 20*time.Millisecond, "sh", "-c", "printf partial; while :; do :; done"); got != "" {
		t.Errorf("timed-out probe returned partial output %q", got)
	}
}
