package cli

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/shell"
)

// run executes Main with stdout and stderr captured.
func run(t *testing.T, version string, args ...string) (stdout, stderr string, code int) {
	t.Helper()
	oldOut, oldErr := os.Stdout, os.Stderr
	rOut, wOut, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	rErr, wErr, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	os.Stdout, os.Stderr = wOut, wErr
	code = Main(args, version)
	_ = wOut.Close()
	_ = wErr.Close()
	os.Stdout, os.Stderr = oldOut, oldErr
	out, _ := io.ReadAll(rOut)
	errOut, _ := io.ReadAll(rErr)
	_ = rOut.Close()
	_ = rErr.Close()
	return string(out), string(errOut), code
}

// setupEnv isolates HOME and the XDG directories inside a temp dir and
// clears argmax- and shell-related environment that could leak into the
// dispatch under test.
func setupEnv(t *testing.T) (home string, paths config.Paths) {
	t.Helper()
	home = t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(home, "xdg-config"))
	t.Setenv("XDG_DATA_HOME", filepath.Join(home, "xdg-data"))
	t.Setenv("XDG_CACHE_HOME", filepath.Join(home, "xdg-cache"))
	for _, env := range []string{
		"ARGMAX_SESSION", "ARGMAX_SHELL", "ARGMAX_HOOK_FD", "ARGMAX_RESCUE",
		"ARGMAX_UPDATE_URL", "ARGMAX_LOG_LEVEL",
		"ARGMAX_CORE_DEBUG",
		"ARGMAX_UI_GHOST_TEXT", "ARGMAX_UI_MAX_SUGGESTIONS", "ARGMAX_UI_MAX_HEIGHT",
		"ARGMAX_UPDATER_INTERVAL", "ARGMAX_UPDATER_CHECK_ON_STARTUP",
		"ARGMAX_AI_ENABLED",
		"ZDOTDIR", "HISTFILE", "SHELL",
	} {
		t.Setenv(env, "")
	}
	// String-typed config overrides use LookupEnv: an empty value would
	// still override, so these must be genuinely unset for the test.
	unsetEnv(t,
		"ARGMAX_CORE_SHELL", "ARGMAX_CORE_MODE",
		"ARGMAX_UPDATER_CHANNEL", "ARGMAX_AI_PROVIDER",
	)
	return home, config.ResolvePaths()
}

// unsetEnv removes environment variables for the duration of a test.
func unsetEnv(t *testing.T, names ...string) {
	t.Helper()
	for _, name := range names {
		if v, ok := os.LookupEnv(name); ok {
			t.Cleanup(func() { _ = os.Setenv(name, v) })
		} else {
			t.Cleanup(func() { _ = os.Unsetenv(name) })
		}
		_ = os.Unsetenv(name)
	}
}

func TestVersion(t *testing.T) {
	setupEnv(t)
	out, _, code := run(t, "1.2.3", "version")
	if code != 0 || out != "argmax 1.2.3\n" {
		t.Fatalf("got code=%d out=%q", code, out)
	}

	out, _, code = run(t, "", "version")
	if code != 0 || out != "argmax dev\n" {
		t.Fatalf("dev build: got code=%d out=%q", code, out)
	}
}

func TestInitValidShells(t *testing.T) {
	setupEnv(t)
	cases := map[string]string{
		"bash": "PROMPT_COMMAND",
		"zsh":  "add-zsh-hook",
		"fish": "fish_prompt",
	}
	for name, marker := range cases {
		out, _, code := run(t, "1.0.0", "init", name)
		if code != 0 {
			t.Fatalf("init %s: code=%d", name, code)
		}
		if !strings.Contains(out, "argmax shell integration") || !strings.Contains(out, marker) {
			t.Fatalf("init %s: unexpected script:\n%s", name, out)
		}
	}
}

func TestInitInvalidShell(t *testing.T) {
	setupEnv(t)
	out, stderr, code := run(t, "1.0.0", "init", "tcsh")
	if code != 2 || out != "" || !strings.Contains(stderr, "unsupported shell") {
		t.Fatalf("got code=%d out=%q stderr=%q", code, out, stderr)
	}

	if _, _, code := run(t, "1.0.0", "init"); code != 2 {
		t.Fatalf("init without shell: code=%d", code)
	}
}

func TestUnknownCommand(t *testing.T) {
	setupEnv(t)
	out, stderr, code := run(t, "1.0.0", "bogus")
	if code != 2 || out != "" {
		t.Fatalf("got code=%d out=%q", code, out)
	}
	if !strings.Contains(stderr, "unknown command") || !strings.Contains(stderr, "usage:") {
		t.Fatalf("stderr missing usage:\n%s", stderr)
	}
}

func TestParseSessionFlags(t *testing.T) {
	opts, err := parseSessionFlags([]string{"--shell", "zsh", "--shell-login", "-d"})
	if err != nil {
		t.Fatal(err)
	}
	if opts.ShellFlag != "zsh" || !opts.Login || !opts.Debug {
		t.Fatalf("got %+v", opts)
	}

	opts, err = parseSessionFlags([]string{"--shell=fish", "-s=ignored-after"})
	if err != nil {
		t.Fatal(err)
	}
	if opts.ShellFlag != "ignored-after" {
		t.Fatalf("--flag=value form: got %+v", opts)
	}

	for _, args := range [][]string{
		{"--shell"},      // missing value
		{"--bogus"},      // unknown flag
		{"--debug=true"}, // boolean with value
		{"positional"},   // not a flag
	} {
		if _, err := parseSessionFlags(args); err == nil {
			t.Errorf("%v must error", args)
		}
	}
}

func TestConfigInit(t *testing.T) {
	_, paths := setupEnv(t)

	out, _, code := run(t, "1.0.0", "config", "init")
	if code != 0 || !strings.Contains(out, paths.ConfigFile) {
		t.Fatalf("got code=%d out=%q", code, out)
	}
	data, err := os.ReadFile(paths.ConfigFile)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != config.DefaultFile {
		t.Fatal("config content is not the default file")
	}
	info, err := os.Stat(paths.ConfigFile)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("config mode = %o, want 600", info.Mode().Perm())
	}

	// Idempotent: a customized file is never clobbered.
	custom := []byte("# customized\n")
	if err := os.WriteFile(paths.ConfigFile, custom, 0o600); err != nil {
		t.Fatal(err)
	}
	out, _, code = run(t, "1.0.0", "config", "init")
	if code != 0 || !strings.Contains(out, "already exists") {
		t.Fatalf("second run: got code=%d out=%q", code, out)
	}
	data, err = os.ReadFile(paths.ConfigFile)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, custom) {
		t.Fatal("config init clobbered an existing file")
	}
}

func TestConfigShow(t *testing.T) {
	_, paths := setupEnv(t)

	cfg := `[ai]
enabled = true
provider = "groq"

[ai.providers.groq]
endpoint = "https://api.groq.com/openai/v1"
api_key = "supersecretkey123"
`
	if err := os.MkdirAll(filepath.Dir(paths.ConfigFile), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.ConfigFile, []byte(cfg), 0o600); err != nil {
		t.Fatal(err)
	}

	out, _, code := run(t, "1.0.0", "config", "show")
	if code != 0 {
		t.Fatalf("code=%d", code)
	}
	if strings.Contains(out, "supersecretkey123") {
		t.Fatal("config show leaked an API key")
	}
	if !strings.Contains(out, "<redacted>") || !strings.Contains(out, "[core]") {
		t.Fatalf("unexpected resolved output:\n%s", out)
	}
}

func TestConfigShowParseError(t *testing.T) {
	_, paths := setupEnv(t)
	if err := os.MkdirAll(filepath.Dir(paths.ConfigFile), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.ConfigFile, []byte("not = [valid"), 0o600); err != nil {
		t.Fatal(err)
	}
	out, stderr, code := run(t, "1.0.0", "config", "show")
	if code == 0 || out != "" || !strings.Contains(stderr, "parse") {
		t.Fatalf("got code=%d out=%q stderr=%q", code, out, stderr)
	}
}

func TestSetupFullFlow(t *testing.T) {
	home, paths := setupEnv(t)
	t.Setenv("SHELL", "/bin/zsh")

	// Pre-existing rc content must survive the managed block.
	wantShell, err := shell.Detect("", "")
	if err != nil {
		t.Fatal(err)
	}
	rc := wantShell.RCFile()
	seed := "# my rc\nexport EDITOR=vim\n"
	if err := os.MkdirAll(filepath.Dir(rc), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(rc, []byte(seed), 0o600); err != nil {
		t.Fatal(err)
	}

	out, stderr, code := run(t, "1.0.0", "setup")
	if code != 0 {
		t.Fatalf("code=%d stderr=%q", code, stderr)
	}

	// Hook block installed in the right rc file, content preserved.
	data, err := os.ReadFile(rc)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), shell.BlockBegin) || !strings.Contains(string(data), "export EDITOR=vim") {
		t.Fatalf("rc file missing block or seed content:\n%s", data)
	}

	// Binary copied with executable permissions and identical bytes.
	bin := filepath.Join(home, ".local", "bin", "argmax")
	info, err := os.Stat(bin)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o755 {
		t.Fatalf("binary mode = %o, want 755", info.Mode().Perm())
	}
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	exeBytes, err := os.ReadFile(exe)
	if err != nil {
		t.Fatal(err)
	}
	binBytes, err := os.ReadFile(bin)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(exeBytes, binBytes) {
		t.Fatal("installed binary differs from the running one")
	}

	// Config created.
	cfgData, err := os.ReadFile(paths.ConfigFile)
	if err != nil {
		t.Fatal(err)
	}
	if string(cfgData) != config.DefaultFile {
		t.Fatal("config content is not the default file")
	}

	// The report names the exact files and the activation command.
	if !strings.Contains(out, rc) || !strings.Contains(out, "source "+rc) {
		t.Fatalf("report missing rc file or activation command:\n%s", out)
	}
	if !strings.Contains(out, bin) || !strings.Contains(out, paths.ConfigFile) {
		t.Fatalf("report missing binary or config path:\n%s", out)
	}

	// Second run: nothing changes and the report says so.
	rcAfter, err := os.ReadFile(rc)
	if err != nil {
		t.Fatal(err)
	}
	out, _, code = run(t, "1.0.0", "setup")
	if code != 0 {
		t.Fatalf("second run: code=%d", code)
	}
	if !strings.Contains(out, "nothing changed") {
		t.Fatalf("second run must report no changes:\n%s", out)
	}
	data, err = os.ReadFile(rc)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, rcAfter) {
		t.Fatal("second run modified the rc file")
	}
}

func TestSetupExplicitShell(t *testing.T) {
	home, _ := setupEnv(t)
	out, stderr, code := run(t, "1.0.0", "setup", "fish")
	if code != 0 {
		t.Fatalf("code=%d stderr=%q", code, stderr)
	}
	rc := filepath.Join(home, "xdg-config", "fish", "config.fish")
	data, err := os.ReadFile(rc)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), shell.BlockBegin) {
		t.Fatalf("fish config missing block:\n%s", data)
	}
	if !strings.Contains(out, rc) {
		t.Fatalf("report missing rc path:\n%s", out)
	}
}

func TestSetupUnsupportedShellTouchesNothing(t *testing.T) {
	home, _ := setupEnv(t)
	_, stderr, code := run(t, "1.0.0", "setup", "tcsh")
	if code == 0 {
		t.Fatal("unsupported shell must fail")
	}
	if !strings.Contains(stderr, "unsupported shell") {
		t.Fatalf("stderr=%q", stderr)
	}
	entries, err := os.ReadDir(home)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("unsupported shell touched files: %v", entries)
	}
}

func TestUninstallRoundTrip(t *testing.T) {
	home, paths := setupEnv(t)

	// Seed an rc file with user content, then set up zsh explicitly.
	rc := filepath.Join(home, ".zshrc")
	seed := "# mine\nexport EDITOR=vim\n"
	if err := os.WriteFile(rc, []byte(seed), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, stderr, code := run(t, "1.0.0", "setup", "zsh"); code != 0 {
		t.Fatalf("setup: code=%d stderr=%q", code, stderr)
	}

	out, stderr, code := run(t, "1.0.0", "uninstall")
	if code != 0 {
		t.Fatalf("code=%d stderr=%q", code, stderr)
	}

	// The block is gone; all other rc content is byte-identical.
	data, err := os.ReadFile(rc)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != seed {
		t.Fatalf("rc content not restored:\n%q", data)
	}

	// Dirs and binary removed and reported.
	for _, dir := range []string{paths.CacheDir, paths.DataDir, filepath.Dir(paths.ConfigFile)} {
		if _, err := os.Stat(dir); !os.IsNotExist(err) {
			t.Errorf("%s still exists", dir)
		}
		if !strings.Contains(out, "removed "+dir) {
			t.Errorf("report missing removal of %s:\n%s", dir, out)
		}
	}
	bin := filepath.Join(home, ".local", "bin", "argmax")
	if _, err := os.Stat(bin); !os.IsNotExist(err) {
		t.Error("binary still present")
	}
	if !strings.Contains(out, "removed "+bin) {
		t.Errorf("report missing removal of %s:\n%s", bin, out)
	}
	if strings.Contains(string(data), "argmax") {
		t.Error("rc file still references argmax")
	}
}

func TestUninstallWarnsInsideSession(t *testing.T) {
	setupEnv(t)
	t.Setenv("ARGMAX_SESSION", "12345")
	_, stderr, code := run(t, "1.0.0", "uninstall")
	if code != 0 {
		t.Fatalf("code=%d", code)
	}
	if !strings.Contains(stderr, "do not kill the parent argmax process") {
		t.Fatalf("missing session warning:\n%s", stderr)
	}
}

func TestCrashLog(t *testing.T) {
	_, paths := setupEnv(t)

	out, _, code := run(t, "1.0.0", "crash-log")
	if code != 0 || !strings.Contains(out, "no crash reports") {
		t.Fatalf("got code=%d out=%q", code, out)
	}

	// With a fixture report the path is printed.
	if err := os.MkdirAll(paths.CrashesDir, 0o700); err != nil {
		t.Fatal(err)
	}
	report := filepath.Join(paths.CrashesDir, "crash-20240101-000000.000.txt")
	if err := os.WriteFile(report, []byte("boom"), 0o600); err != nil {
		t.Fatal(err)
	}
	out, _, code = run(t, "1.0.0", "crash-log")
	if code != 0 || !strings.Contains(out, report) {
		t.Fatalf("got code=%d out=%q", code, out)
	}

	// --clear removes all reports and confirms the count.
	second := filepath.Join(paths.CrashesDir, "crash-20240102-000000.000.txt")
	if err := os.WriteFile(second, []byte("boom"), 0o600); err != nil {
		t.Fatal(err)
	}
	out, _, code = run(t, "1.0.0", "crash-log", "--clear")
	if code != 0 || !strings.Contains(out, "removed 2 crash report(s)") {
		t.Fatalf("got code=%d out=%q", code, out)
	}
	entries, err := os.ReadDir(paths.CrashesDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("reports left behind: %v", entries)
	}
}

func TestReloadNoSession(t *testing.T) {
	_, paths := setupEnv(t)

	out, _, code := run(t, "1.0.0", "reload")
	if code != 0 || !strings.Contains(out, "no active session") {
		t.Fatalf("got code=%d out=%q", code, out)
	}

	// A marker for a dead process changes nothing.
	if err := os.MkdirAll(paths.CacheDir, 0o700); err != nil {
		t.Fatal(err)
	}
	dead := filepath.Join(paths.CacheDir, "session-dead.json")
	if err := os.WriteFile(dead, []byte(`{"pid":99999999,"ppid":1}`), 0o600); err != nil {
		t.Fatal(err)
	}
	out, _, code = run(t, "1.0.0", "reload")
	if code != 0 || !strings.Contains(out, "no active session") {
		t.Fatalf("dead session: got code=%d out=%q", code, out)
	}
}

func TestReloadInvalidConfig(t *testing.T) {
	_, paths := setupEnv(t)
	if err := os.MkdirAll(filepath.Dir(paths.ConfigFile), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(paths.ConfigFile, []byte("[ui]\nmax-height = 2\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	out, stderr, code := run(t, "1.0.0", "reload")
	if code == 0 || out != "" {
		t.Fatalf("got code=%d out=%q", code, out)
	}
	if !strings.Contains(stderr, "ui.max-height") {
		t.Fatalf("error must name the invalid key:\n%s", stderr)
	}
}
