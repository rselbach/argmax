package rank

import (
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func sug(text string, source core.Source, confidence, priority int) Candidate {
	return Candidate{
		Suggestion: core.Suggestion{
			Text:       text,
			Source:     source,
			Confidence: confidence,
			Priority:   priority,
		},
	}
}

func TestBasePriority(t *testing.T) {
	cases := []struct {
		name string
		s    core.Suggestion
		want float64
	}{
		{"explicit priority", core.Suggestion{Source: core.SourceSpec, Priority: 80}, 80},
		{"explicit priority clamped high", core.Suggestion{Source: core.SourceSpec, Priority: 150}, 100},
		{"explicit zero is explicit", core.Suggestion{Source: core.SourceSpec, Priority: 0}, 0},
		{"spec default", core.Suggestion{Source: core.SourceSpec, Priority: -1}, 60},
		{"ai with confidence", core.Suggestion{Source: core.SourceAI, Confidence: 85, Priority: -1}, 85},
		{"ai confidence clamped", core.Suggestion{Source: core.SourceAI, Confidence: 120, Priority: -1}, 100},
		{"ai without confidence", core.Suggestion{Source: core.SourceAI, Priority: -1}, 50},
		{"history with confidence", core.Suggestion{Source: core.SourceHistory, Confidence: 70, Priority: -1}, 70},
		{"history without confidence", core.Suggestion{Source: core.SourceHistory, Priority: -1}, 40},
		{"file generator", core.Suggestion{Source: core.SourceFile, Priority: -1}, 50},
		{"dynamic generator", core.Suggestion{Source: core.SourceDynamic, Priority: -1}, 50},
		{"other source", core.Suggestion{Source: core.SourceSystem, Priority: -1}, 50},
	}
	for _, tc := range cases {
		if got := basePriority(tc.s); got != tc.want {
			t.Errorf("%s: got %v, want %v", tc.name, got, tc.want)
		}
	}
}

func TestContextBonus(t *testing.T) {
	ws := func(sigs ...string) *Workspace {
		w := &Workspace{Signatures: map[string]bool{}}
		for _, s := range sigs {
			w.Signatures[s] = true
		}
		return w
	}
	gitWS := ws(SigGit)
	gitWS.InGit = true
	gitWS.GitBranch = "main"

	cases := []struct {
		text string
		ws   *Workspace
		want float64
	}{
		{"git status", gitWS, 25},
		{"git checkout -b x", gitWS, 25},
		{"git push origin main", gitWS, 60}, // +25 flow, +35 branch reference
		{"git branch main", gitWS, 60},
		{"git init", gitWS, -30},
		{"git clone https://example.com/r.git", gitWS, -30},
		{"git", gitWS, 0},              // bare command, no known flow
		{"git rebase -i", gitWS, 0},    // unlisted subcommand
		{"git status", nil, 0},         // no workspace → no bonuses
		{"git status", ws(SigNode), 0}, // rule requires the git signature
		{"npm run build", ws(SigNode), 20},
		{"pnpm test", ws(SigNode), 20},
		{"yarn add lodash", ws(SigNode), 20},
		{"bun install", ws(SigNode), 20},
		{"npm", ws(SigNode), 0},
		{"go test ./...", ws(SigGo), 20},
		{"go run .", ws(SigGo), 20},
		{"go mod tidy", ws(SigGo), 20},
		{"go mod download", ws(SigGo), 0},
		{"cargo clippy", ws(SigRust), 20},
		{"cargo bench", ws(SigRust), 0},
		{"pytest -x", ws(SigPython), 20},
		{"python3 main.py", ws(SigPython), 20},
		{"pip install requests", ws(SigPython), 20},
		{"poetry add toml", ws(SigPython), 20},
		{"uv run script.py", ws(SigPython), 20},
		{"just build", ws(SigJust), 20},
		{"make", ws(SigMake), 15},
		{"make install", ws(SigMake), 15},
		{"gcc main.c", ws(SigMake), 15},
		{"clang++ -O2 x.cc", ws(SigMake), 0}, // heuristic: only listed spellings match
		{"cmake --build .", ws(SigMake), 15},
		{"docker build .", ws(SigDocker), 20},
		{"docker compose up -d", ws(SigDocker), 20},
		{"docker-compose down", ws(SigDocker), 20},
		{"docker ps", ws(SigDocker), 0},
		{"kubectl get pods", ws(SigKubernetes), 20},
		{"helm upgrade rel ./chart", ws(SigKubernetes), 20},
		{"", gitWS, 0},
	}
	for _, tc := range cases {
		if got := contextBonus(tc.ws, tc.text); got != tc.want {
			t.Errorf("contextBonus(%q): got %v, want %v", tc.text, got, tc.want)
		}
	}
}

func TestContextBonusClamped(t *testing.T) {
	if got := clamp(150, -100, 100); got != 100 {
		t.Fatalf("clamp high: got %v", got)
	}
	if got := clamp(-150, -100, 100); got != -100 {
		t.Fatalf("clamp low: got %v", got)
	}
}

func TestScoreStaticRankingNilEnv(t *testing.T) {
	// nil Store and nil WS must not panic and must rank statically.
	cands := []Candidate{
		sug("zzz-history", core.SourceHistory, 0, -1), // base 40
		sug("aaa-spec", core.SourceSpec, 0, -1),       // base 60
		sug("mmm-file", core.SourceFile, 0, -1),       // base 50
	}
	for i := range cands {
		cands[i].MatchQuality = 100
	}
	out := Score(cands, Env{})
	want := []string{"aaa-spec", "mmm-file", "zzz-history"}
	for i, w := range want {
		if out[i].Suggestion.Text != w {
			t.Fatalf("position %d: got %q, want %q (order %v)", i, out[i].Suggestion.Text, w, texts(out))
		}
	}
	if len(Score(nil, Env{})) != 0 {
		t.Fatal("empty input must yield empty output")
	}
}

func TestScoreWeightsBaseVsFrecency(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	for i := 0; i < 10; i++ {
		s.Record(cwd, "frequent", "", 0)
	}
	// Equal bases: the high-frecency candidate wins (frecency normalizes to
	// 100 for "frequent", 0 for "rare"; 15 > 0 in the weighted total).
	a := sug("frequent", core.SourceSpec, 0, 50)
	b := sug("rare", core.SourceSpec, 0, 50)
	a.Skeleton, b.Skeleton = "frequent", "rare"
	out := Score([]Candidate{b, a}, Env{CWD: cwd, Store: s})
	if out[0].Suggestion.Text != "frequent" {
		t.Fatalf("frecency should decide with equal bases: order %v", texts(out))
	}
	// A wide base-priority gap outweighs frecency: 0.30×(90−10)=24 vs at
	// most 0.15×100=15.
	a = sug("frequent", core.SourceSpec, 0, 10)
	b = sug("rare", core.SourceSpec, 0, 90)
	a.Skeleton, b.Skeleton = "frequent", "rare"
	out = Score([]Candidate{a, b}, Env{CWD: cwd, Store: s})
	if out[0].Suggestion.Text != "rare" {
		t.Fatalf("base priority 90 should beat frecency: order %v", texts(out))
	}
}

func TestScoreNormalizesPerResultSet(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	s.Record(cwd, "one", "", 0)
	s.Record(cwd, "two", "", 0)
	s.Record(cwd, "two", "", 0)
	mk := func(text string) Candidate {
		c := sug(text, core.SourceSpec, 0, 50)
		c.Skeleton = text
		return c
	}
	scored := scoreCandidates([]Candidate{mk("one"), mk("two"), mk("zero")}, Env{CWD: cwd, Store: s})
	byText := map[string]scoredCandidate{}
	for _, sc := range scored {
		byText[sc.Suggestion.Text] = sc
	}
	approxEq(t, byText["two"].frecency, 100) // strongest in the set
	approxEq(t, byText["one"].frecency, 50)  // half of the max
	approxEq(t, byText["zero"].frecency, 0)
	// Weighted totals: 0.30×50 base + 0.15×normalized frecency.
	approxEq(t, byText["two"].total, 15+15)
	approxEq(t, byText["one"].total, 15+7.5)
	approxEq(t, byText["zero"].total, 15)
}

func TestScoreTransitionWeighted(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	s.Record(cwd, "git add", "", 0)
	s.Record(cwd, "git commit", "git add", 0)
	mk := func(text, skel string) Candidate {
		c := sug(text, core.SourceSpec, 0, 50)
		c.Skeleton = skel
		return c
	}
	scored := scoreCandidates([]Candidate{
		mk("git commit", "git commit"),
		mk("git push", "git push"),
	}, Env{CWD: cwd, Store: s, PrevSkeleton: "git add"})
	byText := map[string]scoredCandidate{}
	for _, sc := range scored {
		byText[sc.Suggestion.Text] = sc
	}
	approxEq(t, byText["git commit"].transition, 100)
	approxEq(t, byText["git push"].transition, 0)
	// 0.10×100 transition bonus puts the learned successor first.
	if scored[0].Suggestion.Text != "git commit" {
		t.Fatalf("learned transition should rank first: %v", texts(scoredToCands(scored)))
	}
}

func TestScoreContextBonusAffectsOrder(t *testing.T) {
	ws := &Workspace{Signatures: map[string]bool{SigGit: true}, InGit: true, GitBranch: "main"}
	inGit := sug("git status", core.SourceSpec, 0, 50)
	other := sug("gitsome", core.SourceSpec, 0, 50)
	out := Score([]Candidate{other, inGit}, Env{CWD: "/repo", WS: ws})
	if out[0].Suggestion.Text != "git status" {
		t.Fatalf("context bonus should rank git flow first: %v", texts(out))
	}
}

func TestRanksBeforeTieBreakChain(t *testing.T) {
	mk := func(text string, total, transition, frecency, context float64) scoredCandidate {
		sc := scoredCandidate{Candidate: sug(text, core.SourceSpec, 0, -1)}
		sc.total, sc.transition, sc.frecency, sc.context = total, transition, frecency, context
		return sc
	}
	cases := []struct {
		name string
		a, b scoredCandidate
		want bool // a ranks before b
	}{
		{"total wins", mk("b", 11, 0, 0, 0), mk("a", 10, 99, 99, 99), true},
		{"transition breaks total tie", mk("b", 10, 5, 0, 0), mk("a", 10, 4, 99, 99), true},
		{"frecency breaks transition tie", mk("b", 10, 5, 5, 0), mk("a", 10, 5, 4, 99), true},
		{"context breaks frecency tie", mk("b", 10, 5, 5, 5), mk("a", 10, 5, 5, 4), true},
		{"text breaks full tie", mk("a", 10, 5, 5, 5), mk("b", 10, 5, 5, 5), true},
		{"text breaks full tie reversed", mk("b", 10, 5, 5, 5), mk("a", 10, 5, 5, 5), false},
	}
	for _, tc := range cases {
		if got := ranksBefore(tc.a, tc.b); got != tc.want {
			t.Errorf("%s: got %v, want %v", tc.name, got, tc.want)
		}
	}
}

// TestScoreTieBreakEndToEnd ties weighted totals and checks the chain falls
// through to transition, then to command text.
func TestScoreTieBreakEndToEnd(t *testing.T) {
	s, _ := openTestStore(t)
	cwd := "/repo"
	// A failed execution learns the transition prev→learned without building
	// up any frecency (success count stays 0), so both candidates tie on
	// total: base 15 + one 10-point signal each.
	s.Record(cwd, "learned", "prev", 1)

	// "learned": base 15 + transition 0.10×100 = 25, match 0.
	learned := sug("learned", core.SourceSpec, 0, 50)
	learned.Skeleton = "learned"
	// "matched": base 15 + match 0.20×50 = 25, no transition.
	matched := sug("matched", core.SourceSpec, 0, 50)
	matched.Skeleton = "matched"
	matched.MatchQuality = 50

	out := Score([]Candidate{matched, learned}, Env{CWD: cwd, Store: s, PrevSkeleton: "prev"})
	if out[0].Suggestion.Text != "learned" {
		t.Fatalf("equal totals: transition should break the tie: %v", texts(out))
	}

	// Full tie with no store: falls back to command text ascending.
	x := sug("x-ray", core.SourceSpec, 0, 50)
	y := sug("alpha", core.SourceSpec, 0, 50)
	out = Score([]Candidate{x, y}, Env{})
	if out[0].Suggestion.Text != "alpha" {
		t.Fatalf("full tie: text ascending expected: %v", texts(out))
	}
}

func texts(cands []Candidate) []string {
	out := make([]string, len(cands))
	for i, c := range cands {
		out[i] = c.Suggestion.Text
	}
	return out
}

func scoredToCands(scored []scoredCandidate) []Candidate {
	out := make([]Candidate, len(scored))
	for i := range scored {
		out[i] = scored[i].Candidate
	}
	return out
}
