package rank

import (
	"math"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/rselbach/argmax/internal/core"
)

func approxEq(t *testing.T, got, want float64) {
	t.Helper()
	if math.Abs(got-want) > 1e-9 {
		t.Fatalf("got %v, want ~%v", got, want)
	}
}

func openTestStore(t *testing.T) (*Store, string) {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "argmax")
	s, err := Open(filepath.Join(dir, "history.db"))
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s, dir
}

func TestStoreRecordSuccessIncrementsFrecency(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	if got := s.Frecency(cwd, "git status"); got != 0 {
		t.Fatalf("never seen: got %v, want 0", got)
	}
	s.Record(cwd, "git status", "", 0)
	approxEq(t, s.Frecency(cwd, "git status"), 100)
	s.Record(cwd, "git status", "", 0)
	approxEq(t, s.Frecency(cwd, "git status"), 200)
}

func TestStoreFailedExecutionRefreshesRecencyOnly(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	// A failed execution must not increment the success count: the row exists
	// but bucket × 0 stays 0.
	s.Record(cwd, "make test", "", 1)
	approxEq(t, s.Frecency(cwd, "make test"), 0)
	// A later success makes it positive.
	s.Record(cwd, "make test", "", 0)
	if got := s.Frecency(cwd, "make test"); got <= 0 {
		t.Fatalf("after success: got %v, want > 0", got)
	}

	// Backdate to the ≤7d bucket (frecency 20 × 1), then fail again: recency
	// refreshes (back to the ≤1h bucket) but the success count must not move.
	setLastUsed(t, s, cwd, "make test", time.Now().Add(-2*24*time.Hour))
	approxEq(t, s.Frecency(cwd, "make test"), 20)
	s.Record(cwd, "make test", "", 1)
	approxEq(t, s.Frecency(cwd, "make test"), 100)
}

func TestStoreFrecencyGlobalFallback(t *testing.T) {
	s, _ := openTestStore(t)
	dirA, dirB, dirC := "/repo/a", "/repo/b", "/repo/c"
	s.Record(dirA, "go build", "", 0)
	s.Record(dirA, "go build", "", 0)
	// Exact CWD rows are used at full strength.
	approxEq(t, s.Frecency(dirA, "go build"), 200)
	// Other CWDs see the global aggregate at 70% strength.
	approxEq(t, s.Frecency(dirB, "go build"), 140)
	// The aggregate sums across directories.
	s.Record(dirB, "go build", "", 0)
	approxEq(t, s.Frecency(dirC, "go build"), 0.7*300)
	// Never-seen skeletons score 0.
	approxEq(t, s.Frecency(dirC, "go test"), 0)
}

func TestStoreTransition(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	s.Record(cwd, "git add", "", 0)
	s.Record(cwd, "git commit", "git add", 0)
	approxEq(t, s.Transition(cwd, "git add", "git commit"), 1)
	// Repetition strengthens the learned likelihood.
	s.Record(cwd, "git commit", "git add", 0)
	approxEq(t, s.Transition(cwd, "git add", "git commit"), 2)
	// Other CWDs see global transitions at 70% strength.
	approxEq(t, s.Transition("/elsewhere", "git add", "git commit"), 1.4)
	// Unknown pairs and degenerate inputs score 0.
	approxEq(t, s.Transition(cwd, "git add", "git push"), 0)
	approxEq(t, s.Transition(cwd, "", "git commit"), 0)
	approxEq(t, s.Transition(cwd, "git add", ""), 0)
}

func TestStoreTransitionParentSkeletonFallback(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	// A transition learned from the shallow skeleton "git"…
	s.Record(cwd, "git push", "git", 0)
	// …is found directly…
	approxEq(t, s.Transition(cwd, "git", "git push"), 1)
	// …and via a deep previous skeleton falling back to its parent "git".
	approxEq(t, s.Transition(cwd, "git commit", "git push"), 1)
	// Exact-prev rows win over the parent fallback.
	s.Record(cwd, "git push", "git commit", 0)
	s.Record(cwd, "git push", "git commit", 0)
	approxEq(t, s.Transition(cwd, "git commit", "git push"), 2)
	// A single-token skeleton has no parent and no rows: 0.
	approxEq(t, s.Transition(cwd, "make", "git push"), 0)
}

func TestStoreFilePermissions(t *testing.T) {
	s, dir := openTestStore(t)
	_ = s
	fi, err := os.Stat(filepath.Join(dir, "history.db"))
	if err != nil {
		t.Fatalf("stat db: %v", err)
	}
	if got := fi.Mode().Perm(); got != 0o600 {
		t.Fatalf("db file mode: got %o, want 600", got)
	}
	di, err := os.Stat(dir)
	if err != nil {
		t.Fatalf("stat dir: %v", err)
	}
	if got := di.Mode().Perm(); got != 0o700 {
		t.Fatalf("state dir mode: got %o, want 700", got)
	}
}

func TestStoreCorruptDatabaseRecreated(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "history.db")
	if err := os.WriteFile(path, []byte("this is not a sqlite database"), 0o600); err != nil {
		t.Fatal(err)
	}
	s, err := Open(path)
	if err != nil {
		t.Fatalf("Open on corrupt file: %v", err)
	}
	defer func() { _ = s.Close() }()
	// Methods return zero values without panicking…
	approxEq(t, s.Frecency("/x", "git"), 0)
	approxEq(t, s.Transition("/x", "git", "git status"), 0)
	// …and the recreated database learns normally.
	s.Record("/x", "git status", "", 0)
	if got := s.Frecency("/x", "git status"); got <= 0 {
		t.Fatalf("after recreation: got %v, want > 0", got)
	}
}

func TestStoreNilAndDegradedSafety(t *testing.T) {
	var s *Store
	approxEq(t, s.Frecency("/x", "git"), 0)
	approxEq(t, s.Transition("/x", "git", "git status"), 0)
	s.Record("/x", "git", "git add", 0) // must not panic
	if err := s.Close(); err != nil {
		t.Fatalf("nil Close: %v", err)
	}
	if err := s.Prune(); err != nil {
		t.Fatalf("nil Prune: %v", err)
	}
	d := &Store{} // degraded: no handle
	d.Record("/x", "git", "", 0)
	approxEq(t, d.Frecency("/x", "git"), 0)
}

func TestStoreFrecencyBuckets(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	s.Record(cwd, "make test", "", 0) // success count 1 → frecency == bucket
	cases := []struct {
		age  time.Duration
		want float64
	}{
		{0, 100},
		{time.Hour - time.Minute, 100},   // ≤1h
		{time.Hour + time.Minute, 50},    // >1h, ≤24h
		{24*time.Hour - time.Minute, 50}, // ≤24h
		{24*time.Hour + time.Minute, 20}, // >24h, ≤7d
		{7*24*time.Hour - time.Minute, 20},
		{7*24*time.Hour + time.Minute, 5}, // >7d, ≤30d
		{30*24*time.Hour - time.Minute, 5},
		{30*24*time.Hour + time.Minute, 1}, // >30d
		{90 * 24 * time.Hour, 1},
	}
	for _, tc := range cases {
		setLastUsed(t, s, cwd, "make test", time.Now().Add(-tc.age))
		approxEq(t, s.Frecency(cwd, "make test"), tc.want)
	}
	// Buckets multiply the success count.
	setLastUsed(t, s, cwd, "make test", time.Now())
	s.Record(cwd, "make test", "", 0)
	approxEq(t, s.Frecency(cwd, "make test"), 200)
}

func TestStorePrune(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	s.Record(cwd, "old cmd", "prev old", 0)
	s.Record(cwd, "new cmd", "", 0)
	old := time.Now().Add(-100 * 24 * time.Hour)
	setLastUsed(t, s, cwd, "old cmd", old)
	if _, err := s.db.Exec(`UPDATE transitions SET last_used = ? WHERE cur = ?`, old.Unix(), "old cmd"); err != nil {
		t.Fatalf("backdate transitions: %v", err)
	}
	if err := s.Prune(); err != nil {
		t.Fatalf("Prune: %v", err)
	}
	approxEq(t, s.Frecency(cwd, "old cmd"), 0)
	approxEq(t, s.Frecency("/anywhere", "old cmd"), 0) // globally forgotten
	approxEq(t, s.Transition(cwd, "prev old", "old cmd"), 0)
	if got := s.Frecency(cwd, "new cmd"); got <= 0 {
		t.Fatalf("recent row pruned: got %v, want > 0", got)
	}
}

func setLastUsed(t *testing.T, s *Store, cwd, skeleton string, ts time.Time) {
	t.Helper()
	if _, err := s.db.Exec(`UPDATE usage SET last_used = ? WHERE cwd = ? AND skeleton = ?`,
		ts.Unix(), cwd, skeleton); err != nil {
		t.Fatalf("backdate usage: %v", err)
	}
}

// TestConcurrentRecordAndScore exercises Store and Score from many goroutines;
// run with -race it must be clean (RANK-012 signal collection off the prompt
// path is concurrent with recording after command completion).
func TestConcurrentRecordAndScore(t *testing.T) {
	s, _ := openTestStore(t)
	ws := &Workspace{
		Dir:        "/repo",
		Signatures: map[string]bool{SigGit: true},
		InGit:      true,
		GitBranch:  "main",
	}
	cands := []Candidate{
		{Suggestion: core.Suggestion{Text: "git status", Source: core.SourceSpec}, Skeleton: "git status", MatchQuality: 100},
		{Suggestion: core.Suggestion{Text: "git push origin main", Source: core.SourceSpec}, Skeleton: "git push", MatchQuality: 80},
		{Suggestion: core.Suggestion{Text: "ls", Source: core.SourceSystem}, Skeleton: "ls", MatchQuality: 30},
	}
	var wg sync.WaitGroup
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				s.Record("/repo", "git status", "git add", j%3)
				s.Record("/repo", "git push", "git status", 0)
				_ = s.Frecency("/repo", "git status")
				_ = s.Transition("/repo", "git status", "git push")
			}
		}(i)
	}
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				out := Score(cands, Env{CWD: "/repo", WS: ws, Store: s, PrevSkeleton: "git add"})
				if len(out) != len(cands) {
					return
				}
			}
		}()
	}
	wg.Wait()
}
