package history

import (
	"fmt"
	"testing"
)

// BenchmarkSearch measures tiered fuzzy search over a large history,
// part of the 50 ms p95 suggestion budget (PERF-003).
func BenchmarkSearch(b *testing.B) {
	entries := make([]Entry, 10000)
	for i := range entries {
		entries[i] = Entry{Command: fmt.Sprintf("command-%d --flag value/%d", i, i%97)}
	}
	b.ReportAllocs()
	for b.Loop() {
		if got := Search(entries, "command flag", nil); len(got) == 0 {
			b.Fatal("no matches")
		}
	}
}
