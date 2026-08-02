package input

import "testing"

// BenchmarkDecoderFeed measures per-keystroke decode cost, the local
// bound on the 5 ms p99 input-forwarding budget (PERF-002).
func BenchmarkDecoderFeed(b *testing.B) {
	burst := []byte("git checkout feature/streets-ahead\x1b[A\x1b[B\x1b[3~\x7f\x12é")
	d := &Decoder{}
	b.ReportAllocs()
	for b.Loop() {
		if events := d.Feed(burst); len(events) == 0 {
			b.Fatal("no events decoded")
		}
	}
}
