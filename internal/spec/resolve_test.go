package spec

import (
	"testing"

	"github.com/rselbach/argmax/internal/core"
)

func texts(sugs []core.Suggestion) []string {
	out := make([]string, len(sugs))
	for i, s := range sugs {
		out[i] = s.Text
	}
	return out
}

func contains(ss []string, want string) bool {
	for _, s := range ss {
		if s == want {
			return true
		}
	}
	return false
}

func TestResolveSubcommandPartial(t *testing.T) {
	r := Default()
	res := r.Resolve("git che")
	if res.Root == nil || res.Root.Name != "git" {
		t.Fatalf("Root = %v, want git", res.Root)
	}
	if res.Node != res.Root {
		t.Fatalf("Node = %v, want the git root (no descent while completing)", res.Node)
	}
	if res.Partial != "che" || res.LinePrefix != "git " {
		t.Fatalf("Partial=%q LinePrefix=%q, want che / 'git '", res.Partial, res.LinePrefix)
	}
	if res.Dead || res.MaxedOut || res.Dash {
		t.Fatalf("unexpected flags: Dead=%v MaxedOut=%v Dash=%v", res.Dead, res.MaxedOut, res.Dash)
	}
	got := texts(r.StaticCandidates(res))
	if !contains(got, "git checkout") {
		t.Errorf("StaticCandidates(git che) = %v, want it to contain 'git checkout'", got)
	}
}

func TestResolveGeneratorAfterDescent(t *testing.T) {
	r := Default()
	res := r.Resolve("git checkout ")
	if res.Node == nil || res.Node.Name != "checkout" {
		t.Fatalf("Node = %v, want checkout", res.Node)
	}
	if got := len(res.NodePath); got != 2 || res.NodePath[0] != "git" || res.NodePath[1] != "checkout" {
		t.Fatalf("NodePath = %v, want [git checkout]", res.NodePath)
	}
	if res.Partial != "" || res.LinePrefix != "git checkout " {
		t.Fatalf("Partial=%q LinePrefix=%q", res.Partial, res.LinePrefix)
	}
	id, _, ok := r.GeneratorRequest(res)
	if !ok || id != "git-checkout" {
		t.Fatalf("GeneratorRequest = %q,%v, want git-checkout,true", id, ok)
	}
}

func TestResolveDashOptionsPriority(t *testing.T) {
	r := Default()
	res := r.Resolve("git checkout -")
	if !res.Dash {
		t.Fatal("Dash = false, want true for partial '-'")
	}
	sugs := r.StaticCandidates(res)
	if len(sugs) == 0 {
		t.Fatal("no option candidates for 'git checkout -'")
	}
	for _, s := range sugs {
		if s.Priority != 80 {
			t.Errorf("option %q priority = %d, want 80 (PRD 10.1)", s.Text, s.Priority)
		}
		if s.Confidence != 80 || s.Source != core.SourceSpec {
			t.Errorf("option %q confidence=%d source=%v, want 80/spec", s.Text, s.Confidence, s.Source)
		}
	}
	if !contains(texts(sugs), "git checkout -b") {
		t.Errorf("candidates = %v, want them to contain 'git checkout -b'", texts(sugs))
	}
	// No generator while typing a dash token.
	if id, _, ok := r.GeneratorRequest(res); ok {
		t.Errorf("GeneratorRequest = %q,true, want none while typing an option", id)
	}
}

func TestOptionNotResuggested(t *testing.T) {
	r := Default()
	res := r.Resolve("git checkout --detach -")
	for _, s := range r.StaticCandidates(res) {
		if s.Text == "git checkout --detach --detach" {
			t.Errorf("--detach re-suggested although already present (SPEC-006)")
		}
	}
	// Non-dash position: options still suggested at low priority, minus the
	// ones already present.
	res2 := r.Resolve("git checkout --detach ")
	foundDetach := false
	foundOther := false
	for _, s := range r.StaticCandidates(res2) {
		if s.Priority != 10 {
			t.Errorf("option %q priority = %d, want 10 while typing a normal argument", s.Text, s.Priority)
		}
		if s.Text == "git checkout --detach --detach" {
			foundDetach = true
		}
		if s.Text == "git checkout --detach --force" {
			foundOther = true
		}
	}
	if foundDetach {
		t.Error("--detach re-suggested in normal-argument position")
	}
	if !foundOther {
		t.Error("--force missing in normal-argument position")
	}
}

func TestTraversalSkipsFlagArguments(t *testing.T) {
	r := Default()
	res := r.Resolve(`git commit --amend -m "x" `)
	if res.Dead {
		t.Fatal("Dead = true, want false")
	}
	if res.Node == nil || res.Node.Name != "commit" {
		t.Fatalf("Node = %v, want commit (flag argument must be skipped, SPEC-004)", res.Node)
	}
	if len(res.Args) != 0 {
		t.Fatalf("Args = %v, want none (the -m value is a flag argument)", res.Args)
	}
	for _, s := range r.StaticCandidates(res) {
		if s.Text == "git commit --amend" || s.Text == "git commit --message" {
			t.Errorf("already-present option re-suggested: %q", s.Text)
		}
	}
}

func TestMaxedOut(t *testing.T) {
	r := Default()
	res := r.Resolve("git branch main ")
	if !res.MaxedOut {
		t.Fatal("MaxedOut = false, want true (branch MaxArgs 1, one arg given)")
	}
	if id, _, ok := r.GeneratorRequest(res); ok {
		t.Errorf("GeneratorRequest = %q,true, want nothing once maxed out (SPEC-005)", id)
	}
	// One arg still in progress: not yet maxed out.
	res2 := r.Resolve("git branch ma")
	if res2.MaxedOut {
		t.Error("MaxedOut = true while the first arg is still being typed")
	}
	if id, _, ok := r.GeneratorRequest(res2); !ok || id != "git-branches" {
		t.Errorf("GeneratorRequest = %q,%v, want git-branches,true", id, ok)
	}
}

func TestDead(t *testing.T) {
	r := Default()
	res := r.Resolve("git frobnicate x")
	if !res.Dead {
		t.Fatal("Dead = false, want true (git requires a subcommand, SPEC-009)")
	}
	for _, s := range r.StaticCandidates(res) {
		// Subcommands must be suppressed; only option candidates may remain.
		for _, sub := range res.Node.Subcommands {
			if s.Text == res.LinePrefix+sub.Name {
				t.Errorf("subcommand %q suggested for a dead line", s.Text)
			}
		}
	}
	if id, _, ok := r.GeneratorRequest(res); ok {
		t.Errorf("GeneratorRequest = %q,true, want nothing for a dead line", id)
	}

	// A node without subcommands accepts positionals instead of going dead.
	res2 := r.Resolve("git checkout xyz")
	if res2.Dead {
		t.Error("Dead = true for a positional at a leaf node")
	}
}

func TestResolveCaseInsensitive(t *testing.T) {
	r := Default()
	res := r.Resolve("GIT CHE")
	if res.Root == nil || res.Root.Name != "git" {
		t.Fatalf("Root = %v, want git for 'GIT CHE'", res.Root)
	}
	got := texts(r.StaticCandidates(res))
	if !contains(got, "GIT checkout") {
		t.Errorf("StaticCandidates(GIT CHE) = %v, want it to preserve the typed text as 'GIT checkout'", got)
	}
}

func TestResolveUnknown(t *testing.T) {
	r := Default()
	res := r.Resolve("nosuchcmd foo")
	if res.Root != nil || res.Node != nil {
		t.Fatalf("Root/Node = %v/%v, want nil for an unknown command", res.Root, res.Node)
	}
	if res.Dead {
		t.Error("Dead = true, want false for an unknown command")
	}
	if res.Partial != "foo" || res.LinePrefix != "nosuchcmd " {
		t.Errorf("Partial=%q LinePrefix=%q", res.Partial, res.LinePrefix)
	}
	if got := r.StaticCandidates(res); got != nil {
		t.Errorf("StaticCandidates = %v, want nil for an unknown command", got)
	}
}

func TestGeneratorRequestOptionArgument(t *testing.T) {
	r := Default()

	// Completing the argument of a known option that takes a generated arg.
	res := r.Resolve("ssh -i ")
	id, _, ok := r.GeneratorRequest(res)
	if !ok || id != "files" {
		t.Errorf("GeneratorRequest(ssh -i ) = %q,%v, want files,true", id, ok)
	}

	// The partial exactly naming such an option (SPEC-008 dash case).
	res = r.Resolve("ssh -i")
	id, _, ok = r.GeneratorRequest(res)
	if !ok || id != "files" {
		t.Errorf("GeneratorRequest(ssh -i) = %q,%v, want files,true", id, ok)
	}

	// An option argument without a generator suppresses the node generator:
	// `git checkout -b` creates a new branch, so existing branches must not
	// be suggested (PRD 9.8 branch creation flags).
	res = r.Resolve("git checkout -b ")
	if id, _, ok := r.GeneratorRequest(res); ok {
		t.Errorf("GeneratorRequest(git checkout -b ) = %q,true, want suppressed", id)
	}
	// After the branch name is complete, checkout's generator works again.
	res = r.Resolve("git checkout -b topic ")
	id, _, ok = r.GeneratorRequest(res)
	if !ok || id != "git-checkout" {
		t.Errorf("GeneratorRequest(git checkout -b topic ) = %q,%v, want git-checkout,true", id, ok)
	}
}

func TestGeneratorRequestArgs(t *testing.T) {
	r := Default()
	res := r.Resolve("git push origin ")
	id, args, ok := r.GeneratorRequest(res)
	if !ok || id != "git-pushpull" {
		t.Fatalf("GeneratorRequest = %q,%v, want git-pushpull,true", id, ok)
	}
	if len(args) != 1 || args[0] != "origin" {
		t.Fatalf("args = %v, want [origin]", args)
	}
}

func TestResolveNameValueSkipped(t *testing.T) {
	r := Default()
	res := r.Resolve("git log FOO=bar --oneline ")
	if res.Dead {
		t.Fatal("Dead = true, want false (name=value tokens are skipped)")
	}
	if res.Node == nil || res.Node.Name != "log" {
		t.Fatalf("Node = %v, want log", res.Node)
	}
	if len(res.Args) != 0 {
		t.Fatalf("Args = %v, want none", res.Args)
	}
}

func TestTopLevel(t *testing.T) {
	r := Default()
	sugs := r.TopLevel("gi")
	if !contains(texts(sugs), "git") {
		t.Fatalf("TopLevel(gi) = %v, want it to contain git", texts(sugs))
	}
	for _, s := range sugs {
		if s.Confidence != 90 || s.Source != core.SourceSpec {
			t.Errorf("TopLevel candidate %q confidence=%d source=%v, want 90/spec", s.Text, s.Confidence, s.Source)
		}
		if s.Priority != 60 {
			t.Errorf("TopLevel candidate %q priority = %d, want the 60 default", s.Text, s.Priority)
		}
	}
	// Empty partial returns the whole catalog.
	if got := len(r.TopLevel("")); got != 566 {
		t.Errorf("TopLevel('') returned %d specs, want 566", got)
	}
	// Case-insensitive prefix.
	if !contains(texts(r.TopLevel("GI")), "git") {
		t.Error("TopLevel(GI) missing git")
	}
}

func TestLookupAndNames(t *testing.T) {
	r := Default()
	if r.Lookup("GIT") == nil {
		t.Error("Lookup(GIT) = nil, want the git spec (case-insensitive)")
	}
	if r.Lookup("npm") == nil {
		t.Error("Lookup(npm) = nil")
	}
	if r.Lookup("definitely-not-a-command") != nil {
		t.Error("Lookup of an unknown name must be nil")
	}
	all := r.All()
	names := r.Names()
	if len(all) != len(names) {
		t.Fatalf("All() has %d specs, Names() has %d", len(all), len(names))
	}
	for i := 1; i < len(names); i++ {
		if names[i-1] > names[i] {
			t.Fatalf("Names() not sorted at %d: %q > %q", i, names[i-1], names[i])
		}
	}
}
