package rank_test

import (
	"os/exec"
	"testing"

	"github.com/rselbach/argmax/internal/catalog"
	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/rank"
)

func TestRankCheckoutOrder(t *testing.T) {
	dir := t.TempDir()
	run := func(args ...string) {
		cmd := exec.Command("git", args...)
		cmd.Dir = dir
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	run("init", "-q", "-b", "main")
	run("-c", "user.email=troy@greendale.edu", "-c", "user.name=Troy Barnes", "commit", "-q", "--allow-empty", "-m", "init")
	run("branch", "feature/streets-ahead")

	reg := catalog.Registry()
	e := &complete.Engine{Registry: reg}
	line := "git checkout "
	cands := e.Complete(complete.Context{CWD: dir, GitFilterActiveBranch: true}, line)
	cands = complete.Dedupe(cands, line)
	det := &rank.Detector{}
	ranked := rank.Rank(cands, line, rank.Signals{Workspace: det.Detect(dir), Skeleton: reg.Skeleton})
	for i, c := range ranked {
		if i > 4 {
			break
		}
		t.Logf("%2d. %-45s prio=%d src=%s", i, c.Text, c.Priority, c.Source)
	}
	if len(ranked) == 0 || ranked[0].Text != "git checkout feature/streets-ahead" {
		t.Errorf("branch should outrank options, got first = %q", ranked[0].Text)
	}
}

// BenchmarkRankedCompletion measures the spec-mode suggestion pipeline
// (engine traversal plus adaptive ranking) against the full catalog,
// the core of the 50 ms p95 render budget (PERF-003).
func BenchmarkRankedCompletion(b *testing.B) {
	reg := catalog.Registry()
	e := &complete.Engine{Registry: reg}
	det := &rank.Detector{}
	dir := b.TempDir()
	ctx := complete.Context{CWD: dir}
	sig := rank.Signals{Workspace: det.Detect(dir), Skeleton: reg.Skeleton}
	b.ReportAllocs()
	for b.Loop() {
		cands := e.Complete(ctx, "git che")
		cands = complete.Dedupe(cands, "git che")
		if got := rank.Rank(cands, "git che", sig); len(got) == 0 {
			b.Fatal("no candidates")
		}
	}
}
