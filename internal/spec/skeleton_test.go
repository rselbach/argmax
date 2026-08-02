package spec

import "testing"

func TestSkeleton(t *testing.T) {
	r := Default()
	cases := []struct {
		line, want string
	}{
		{"git checkout -b foo", "git checkout"},
		{"git checkout", "git checkout"},
		{"git checkout ", "git checkout"},
		{`git commit -m "fix the thing"`, "git commit"},
		{"git push -u origin main", "git push"},
		{"git -C repo status", "git status"}, // global flag argument skipped
		{"docker compose up -d", "docker compose up"},
		{"kubectl -n prod get pods", "kubectl get"},
		{"GIT CHECKOUT foo", "git checkout"}, // case-insensitive, canonical form
		{"foo bar", "foo"},                   // unknown: bare executable name
		{"", ""},
		{"   ", ""},
	}
	for _, c := range cases {
		if got := r.Skeleton(c.line); got != c.want {
			t.Errorf("Skeleton(%q) = %q, want %q", c.line, got, c.want)
		}
	}
}

func TestSkeletonAliasesResolveCanonically(t *testing.T) {
	// "gco"-style shell aliases registered as spec aliases must resolve to
	// the canonical command and subcommand names (SPEC-010).
	r := buildRegistry([]*Spec{
		{
			Name:    "git",
			Aliases: []string{"gco"},
			Subcommands: []*Spec{
				{
					Name:    "checkout",
					Aliases: []string{"co"},
					Options: []Option{optVal("new branch", "-b")},
				},
				{Name: "commit", Aliases: []string{"ci"}},
			},
		},
	})
	if got := r.Skeleton("gco co -b foo"); got != "git checkout" {
		t.Errorf("Skeleton(gco co -b foo) = %q, want 'git checkout'", got)
	}
	if got := r.Skeleton("gco ci"); got != "git commit" {
		t.Errorf("Skeleton(gco ci) = %q, want 'git commit'", got)
	}
	// The alias also works for normal resolution.
	if s := r.Lookup("GCO"); s == nil || s.Name != "git" {
		t.Errorf("Lookup(GCO) = %v, want the git spec", s)
	}
}
