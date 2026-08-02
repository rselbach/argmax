package complete

import (
	"strings"
	"testing"
)

func testRegistry() *Registry {
	branchGen := func(_ Context, _ []string, prefix string) []Candidate {
		var out []Candidate
		for _, b := range []string{"main", "feature/login", "fix bug"} {
			if hasFoldPrefix(b, prefix) {
				out = append(out, Candidate{Title: b, Description: "branch"})
			}
		}
		return out
	}
	return NewRegistry(
		&Spec{
			Name: "git", Description: "version control",
			Options: []Option{
				{Names: []string{"-C"}, Description: "path", TakesArg: true},
			},
			Subcommands: []*Spec{
				{Name: "checkout", Aliases: []string{"co"}, Description: "switch branches", Generator: branchGen,
					Options: []Option{
						{Names: []string{"-b"}, Description: "new branch", TakesArg: true},
						{Names: []string{"-f", "--force"}, Description: "force"},
					}},
				{Name: "cherry-pick", Description: "apply commits"},
				{Name: "status", Description: "working tree status"},
				{Name: "push", Description: "update remotes", MaxArgs: 2, Generator: func(_ Context, args []string, prefix string) []Candidate {
					switch len(args) {
					case 0:
						return []Candidate{{Title: "origin"}}
					case 1:
						return []Candidate{{Title: "main"}}
					}
					return nil
				}},
			},
		},
	)
}

func findTitles(cands []Candidate) []string {
	out := make([]string, 0, len(cands))
	for _, c := range cands {
		out = append(out, c.Title)
	}
	return out
}

func contains(cands []Candidate, text string) bool {
	for _, c := range cands {
		if c.Text == text {
			return true
		}
	}
	return false
}

func TestEngineComplete(t *testing.T) {
	e := &Engine{Registry: testRegistry()}
	ctx := Context{CWD: "/tmp"}

	tests := map[string]struct {
		line       string
		wantText   []string
		rejectText []string
	}{
		"subcommand prefix": {
			line:     "git che",
			wantText: []string{"git checkout", "git cherry-pick"},
		},
		"case-insensitive subcommand prefix": {
			line:     "git CHE",
			wantText: []string{"git checkout"},
		},
		"alias reaches subcommand": {
			line:     "git co",
			wantText: []string{"git checkout"},
		},
		"generator values": {
			line:     "git checkout ",
			wantText: []string{"git checkout main", "git checkout feature/login"},
		},
		"generator with quoted multiword": {
			line:     "git checkout fix",
			wantText: []string{`git checkout "fix bug"`},
		},
		"option promotion while typing dash": {
			line:     "git checkout --f",
			wantText: []string{"git checkout --force"},
		},
		"used option not suggested again": {
			line:       "git checkout --force --f",
			rejectText: []string{"git checkout --force --force"},
		},
		"flag argument skipped in traversal": {
			line:     "git -C /tmp che",
			wantText: []string{"git -C /tmp checkout"},
		},
		"max args stops positional suggestions": {
			line:       "git push origin main ",
			rejectText: []string{"git push origin main origin"},
		},
		"invalid deeper token suppresses completion": {
			line:       "git bogus ",
			rejectText: []string{"git bogus status"},
		},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			got := e.Complete(ctx, tc.line)
			for _, want := range tc.wantText {
				if !contains(got, want) {
					t.Errorf("Complete(%q) missing %q; got titles %v", tc.line, want, findTitles(got))
				}
			}
			for _, reject := range tc.rejectText {
				if contains(got, reject) {
					t.Errorf("Complete(%q) should not contain %q", tc.line, reject)
				}
			}
		})
	}
}

func TestOptionPriorities(t *testing.T) {
	e := &Engine{Registry: testRegistry()}
	got := e.Complete(Context{}, "git checkout -")
	for _, c := range got {
		if strings.HasPrefix(c.Title, "-") && c.Priority != 80 {
			t.Errorf("option %q priority = %d while typing dash, want 80", c.Title, c.Priority)
		}
	}
	got = e.Complete(Context{}, "git checkout ma")
	for _, c := range got {
		if strings.HasPrefix(c.Title, "-") && c.Priority != 10 {
			t.Errorf("option %q priority = %d while typing argument, want 10", c.Title, c.Priority)
		}
	}
}

func TestSkeleton(t *testing.T) {
	r := testRegistry()
	tests := map[string]struct {
		line string
		want string
	}{
		"nested":            {line: "git checkout main", want: "git checkout"},
		"alias":             {line: "git co main", want: "git checkout"},
		"flags ignored":     {line: "git -C /tmp checkout -b topic", want: "git checkout"},
		"unknown command":   {line: "unknowncmd --flag", want: "unknowncmd"},
		"root only":         {line: "git", want: "git"},
		"name=value tokens": {line: "git checkout FOO=bar", want: "git checkout"},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			if got := r.Skeleton(tc.line); got != tc.want {
				t.Errorf("Skeleton(%q) = %q, want %q", tc.line, got, tc.want)
			}
		})
	}
}

func TestParentSkeleton(t *testing.T) {
	if got := ParentSkeleton("git checkout"); got != "git" {
		t.Errorf("ParentSkeleton(git checkout) = %q, want git", got)
	}
	if got := ParentSkeleton("git"); got != "" {
		t.Errorf("ParentSkeleton(git) = %q, want empty", got)
	}
}

func TestDedupe(t *testing.T) {
	cands := []Candidate{
		{Text: "git status", Confidence: 50},
		{Text: "git status", Source: SourceAI, Confidence: 85},
		{Text: "git diff"},
		{Text: "gs", Source: SourceAlias},
	}
	got := Dedupe(cands, "gs")
	if len(got) != 3 {
		t.Fatalf("Dedupe returned %d candidates, want 3", len(got))
	}
	if got[0].Source != SourceAI {
		t.Errorf("higher-confidence AI duplicate should replace the row; got source %q", got[0].Source)
	}
	if got[2].Text != "gs" {
		t.Errorf("alias equal to the query must be kept, got %q", got[2].Text)
	}
	// A non-alias exact copy of the query is dropped.
	got = Dedupe([]Candidate{{Text: "git status"}}, "git status")
	if len(got) != 0 {
		t.Errorf("exact query copy should be dropped, got %v", got)
	}
}

func TestRegistryValidate(t *testing.T) {
	tests := map[string]struct {
		registry *Registry
		wantErr  bool
	}{
		"valid":        {registry: testRegistry(), wantErr: false},
		"duplicate":    {registry: NewRegistry(&Spec{Name: "x"}, &Spec{Name: "X"}), wantErr: true},
		"empty sub":    {registry: NewRegistry(&Spec{Name: "x", Subcommands: []*Spec{{Name: ""}}}), wantErr: true},
		"bad priority": {registry: NewRegistry(&Spec{Name: "x", Priority: 150}), wantErr: true},
	}
	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			err := tc.registry.Validate()
			if (err != nil) != tc.wantErr {
				t.Errorf("Validate() error = %v, wantErr %v", err, tc.wantErr)
			}
		})
	}
}

func TestPendingOptionArgument(t *testing.T) {
	dirGen := func(_ Context, _ []string, prefix string) []Candidate {
		if hasFoldPrefix("srcdir", prefix) {
			return []Candidate{{Title: "srcdir", IsDirectory: true}}
		}
		return nil
	}
	reg := NewRegistry(&Spec{
		Name: "tool",
		Subcommands: []*Spec{
			{Name: "run", Generator: func(_ Context, _ []string, prefix string) []Candidate {
				return []Candidate{{Title: "positional-value"}}
			}, Options: []Option{
				{Names: []string{"-b"}, Description: "new name", TakesArg: true},
				{Names: []string{"-C"}, Description: "directory", TakesArg: true, Generator: dirGen},
			}},
		},
	})
	e := &Engine{Registry: reg}

	// A pending argument for an option without a generator suggests
	// nothing: the node generator must stay suppressed.
	if got := e.Complete(Context{}, "tool run -b "); len(got) != 0 {
		t.Errorf("pending -b argument should suppress node suggestions, got %v", findTitles(got))
	}
	if got := e.Complete(Context{}, "tool run -b par"); len(got) != 0 {
		t.Errorf("typing -b's argument should suppress node suggestions, got %v", findTitles(got))
	}
	// An option generator serves the pending argument.
	got := e.Complete(Context{}, "tool run -C src")
	if !contains(got, "tool run -C srcdir") {
		t.Errorf("option generator should complete -C argument, got %v", findTitles(got))
	}
	// Once the argument is supplied, node completion resumes.
	got = e.Complete(Context{}, "tool run -b name ")
	if !contains(got, "tool run -b name positional-value") {
		t.Errorf("node generator should resume after the option argument, got %v", findTitles(got))
	}
}
