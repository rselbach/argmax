package sources

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestCobraCompleteRefusesSeparators(t *testing.T) {
	s := newTestSources()
	for _, name := range []string{"../evil", "a/b", `a\b`, "/bin/ls", ""} {
		if got := s.CobraComplete(context.Background(), name, nil, ""); got != nil {
			t.Fatalf("CobraComplete(%q) = %v, want nil", name, collect(got))
		}
	}
}

func TestCobraCompleteUnknownBinary(t *testing.T) {
	s := newTestSources()
	if got := s.CobraComplete(context.Background(), "argmax-no-such-binary-xyz", nil, ""); got != nil {
		t.Fatalf("got %v, want nil", collect(got))
	}
}

func TestParseCobraOutput(t *testing.T) {
	out := "foo\tFoo desc\nbar\t\n_private\tinternal\n:4\nCompletion ended with directive: ShellCompDirectiveNoDesc\n"
	res := parseCobraOutput([]byte(out), "")
	if len(res) != 2 {
		t.Fatalf("got %v, want [foo bar]", collect(res))
	}
	if res[0].Text != "foo" || res[0].Description != "Foo desc" {
		t.Fatalf("foo = %+v", res[0])
	}
	if res[0].Confidence != 65 || res[0].Source != "inferred" {
		t.Fatalf("metadata = %+v", res[0])
	}

	// Case-insensitive partial filter.
	res = parseCobraOutput([]byte("Foo\td\nbar\td\n:0\n"), "f")
	if len(res) != 1 || res[0].Text != "Foo" {
		t.Fatalf("partial filter: %v", collect(res))
	}

	// Error directive yields nil.
	if res := parseCobraOutput([]byte("foo\td\n:1\n"), ""); res != nil {
		t.Fatalf("error directive: %v", collect(res))
	}

	// Malformed lines are ignored (an empty value before the tab is skipped).
	res = parseCobraOutput([]byte("\n\n\ta\nok\tfine\n:4\n"), "")
	if len(res) != 1 || res[0].Text != "ok" {
		t.Fatalf("malformed: %v", collect(res))
	}
}

func TestCobraCompleteFakeCLI(t *testing.T) {
	bin := t.TempDir()
	script := `#!/bin/sh
if [ "$1" = "__complete" ]; then
  printf 'foo\tFoo desc\nbar\tBar desc\n:4\n'
  exit 0
fi
exit 1
`
	path := filepath.Join(bin, "fakecobra")
	if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))

	s := newTestSources()
	res := s.CobraComplete(context.Background(), "fakecobra", nil, "")
	if len(res) != 2 {
		t.Fatalf("got %v, want [foo bar]", collect(res))
	}
	// Partial filtering applies.
	res = s.CobraComplete(context.Background(), "fakecobra", nil, "f")
	if len(res) != 1 || res[0].Text != "foo" || res[0].Description != "Foo desc" {
		t.Fatalf("partial: %v", collect(res))
	}
	// Second call exercises the cache path.
	res = s.CobraComplete(context.Background(), "fakecobra", nil, "f")
	if len(res) != 1 || res[0].Text != "foo" {
		t.Fatalf("cached: %v", collect(res))
	}
}

func TestCobraCompleteCacheInvalidation(t *testing.T) {
	bin := t.TempDir()
	path := filepath.Join(bin, "fakecobra2")
	write := func(out string) {
		t.Helper()
		script := "#!/bin/sh\nif [ \"$1\" = \"__complete\" ]; then\n  printf '" + out + "'\n  exit 0\nfi\nexit 1\n"
		if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	write("one\\tfirst\\n:4\\n")
	t.Setenv("PATH", bin+string(os.PathListSeparator)+os.Getenv("PATH"))

	s := newTestSources()
	res := s.CobraComplete(context.Background(), "fakecobra2", nil, "")
	if len(res) != 1 || res[0].Text != "one" {
		t.Fatalf("first: %v", collect(res))
	}
	// Replace the binary (changed mtime/size) — the cache key changes.
	write("two\\tsecond\\n:4\\n")
	res = s.CobraComplete(context.Background(), "fakecobra2", nil, "")
	if len(res) != 1 || res[0].Text != "two" {
		t.Fatalf("replaced binary should invalidate cache: %v", collect(res))
	}
}
