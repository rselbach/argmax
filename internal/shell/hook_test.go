package shell

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// setupEnv points HOME and the XDG/Zsh variables at a temp directory and
// returns it.
func setupEnv(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZDOTDIR", "")
	t.Setenv("XDG_CONFIG_HOME", "")
	return home
}

func readFile(t *testing.T, path string) string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	return string(data)
}

func TestInstallHookCreatesFile(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")

	file, changed, err := InstallHook(Bash)
	if err != nil {
		t.Fatalf("InstallHook: %v", err)
	}
	if !changed {
		t.Fatal("first install should report changed=true")
	}
	if file != rc {
		t.Fatalf("file = %q, want %q", file, rc)
	}

	info, err := os.Stat(rc)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o, want 600", info.Mode().Perm())
	}

	content := readFile(t, rc)
	if !strings.HasPrefix(content, BlockBegin+"\n") {
		t.Fatalf("content should start with the begin marker:\n%q", content)
	}
	if !strings.HasSuffix(content, BlockEnd+"\n") {
		t.Fatalf("content should end with the end marker:\n%q", content)
	}
	if !strings.Contains(content, strings.TrimRight(Bash.InitScript(), "\n")) {
		t.Fatal("content should contain the full init script")
	}

	// Idempotent: identical bytes, changed=false.
	before := content
	_, changed, err = InstallHook(Bash)
	if err != nil {
		t.Fatalf("second InstallHook: %v", err)
	}
	if changed {
		t.Fatal("second install should be a no-op")
	}
	if after := readFile(t, rc); after != before {
		t.Fatal("second install must not change the bytes")
	}
}

func TestInstallHookPreservesExistingContentAndMode(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")
	original := "# my stuff\nalias ll='ls -l'\n"
	if err := os.WriteFile(rc, []byte(original), 0o644); err != nil {
		t.Fatal(err)
	}

	_, changed, err := InstallHook(Bash)
	if err != nil || !changed {
		t.Fatalf("InstallHook: changed=%v err=%v", changed, err)
	}
	content := readFile(t, rc)
	if !strings.HasPrefix(content, original) {
		t.Fatalf("original content must be preserved at the top:\n%q", content)
	}
	if !strings.Contains(content, "\n"+BlockBegin+"\n") {
		t.Fatalf("block should be separated by a blank line:\n%q", content)
	}
	info, _ := os.Stat(rc)
	if info.Mode().Perm() != 0o644 {
		t.Fatalf("mode = %o, want preserved 644", info.Mode().Perm())
	}
}

func TestInstallHookReplacesOutdatedBlock(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".zshrc")
	original := "# header\n"
	if err := os.WriteFile(rc, []byte(original), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := InstallHook(Zsh); err != nil {
		t.Fatalf("install: %v", err)
	}

	// Simulate an outdated integration by editing inside the markers.
	content := readFile(t, rc)
	outdated := strings.Replace(content, "argmax shell integration for zsh.", "old integration", 1)
	if outdated == content {
		t.Fatal("fixture setup failed to modify the block")
	}
	if err := os.WriteFile(rc, []byte(outdated), 0o644); err != nil {
		t.Fatal(err)
	}

	_, changed, err := InstallHook(Zsh)
	if err != nil {
		t.Fatalf("upgrade install: %v", err)
	}
	if !changed {
		t.Fatal("outdated block should be replaced")
	}
	got := readFile(t, rc)
	if got != content {
		t.Fatalf("upgrade should restore the current block exactly\ngot:  %q\nwant: %q", got, content)
	}

	// And now it is a no-op again.
	if _, changed, err = InstallHook(Zsh); err != nil || changed {
		t.Fatalf("post-upgrade install: changed=%v err=%v", changed, err)
	}
}

func TestRemoveHookRestoresOriginal(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")
	original := "# my stuff\nalias ll='ls -l'\n\nexport EDITOR=vim\n"
	if err := os.WriteFile(rc, []byte(original), 0o640); err != nil {
		t.Fatal(err)
	}
	if _, _, err := InstallHook(Bash); err != nil {
		t.Fatalf("install: %v", err)
	}

	_, changed, err := RemoveHook(Bash)
	if err != nil {
		t.Fatalf("RemoveHook: %v", err)
	}
	if !changed {
		t.Fatal("remove should report changed=true")
	}
	if got := readFile(t, rc); got != original {
		t.Fatalf("remove should restore the exact original\ngot:  %q\nwant: %q", got, original)
	}
	info, _ := os.Stat(rc)
	if info.Mode().Perm() != 0o640 {
		t.Fatalf("mode = %o, want preserved 640", info.Mode().Perm())
	}

	// Removing again is a no-op.
	if _, changed, err = RemoveHook(Bash); err != nil || changed {
		t.Fatalf("second remove: changed=%v err=%v", changed, err)
	}
	if got := readFile(t, rc); got != original {
		t.Fatal("second remove must not change the bytes")
	}
}

func TestRemoveHookNoBlock(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")

	// Missing file: no change, no error.
	if _, changed, err := RemoveHook(Bash); err != nil || changed {
		t.Fatalf("missing file: changed=%v err=%v", changed, err)
	}

	// File without a block: untouched.
	content := "# nothing managed here\n"
	if err := os.WriteFile(rc, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, changed, err := RemoveHook(Bash); err != nil || changed {
		t.Fatalf("no block: changed=%v err=%v", changed, err)
	}
	if got := readFile(t, rc); got != content {
		t.Fatal("file without a block must be untouched")
	}
}

func TestRemoveHookOnlyTheBlock(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")
	if err := os.WriteFile(rc, []byte("# before\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := InstallHook(Bash); err != nil {
		t.Fatalf("install: %v", err)
	}
	// Content after the block must survive removal.
	after := readFile(t, rc) + "# after\n"
	if err := os.WriteFile(rc, []byte(after), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, changed, err := RemoveHook(Bash); err != nil || !changed {
		t.Fatalf("remove: changed=%v err=%v", changed, err)
	}
	got := readFile(t, rc)
	if strings.Contains(got, "argmax") {
		t.Fatalf("block should be gone:\n%q", got)
	}
	if !strings.Contains(got, "# before\n") || !strings.Contains(got, "# after\n") {
		t.Fatalf("unrelated content must survive:\n%q", got)
	}
}

func TestInstallHookFishXDG(t *testing.T) {
	home := t.TempDir()
	xdg := filepath.Join(t.TempDir(), "xdg")
	t.Setenv("HOME", home)
	t.Setenv("XDG_CONFIG_HOME", xdg)

	file, changed, err := InstallHook(Fish)
	if err != nil || !changed {
		t.Fatalf("InstallHook: changed=%v err=%v", changed, err)
	}
	want := filepath.Join(xdg, "fish", "config.fish")
	if file != want {
		t.Fatalf("file = %q, want %q", file, want)
	}
	info, err := os.Stat(file)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o, want 600", info.Mode().Perm())
	}
	if content := readFile(t, file); !strings.Contains(content, BlockBegin) {
		t.Fatal("block should be installed")
	}

	// Round trip: removal of the only content leaves an empty file.
	if _, changed, err := RemoveHook(Fish); err != nil || !changed {
		t.Fatalf("remove: changed=%v err=%v", changed, err)
	}
	if got := readFile(t, file); got != "" {
		t.Fatalf("removing the only content should leave an empty file, got %q", got)
	}
}

func TestInstallHookZdotdir(t *testing.T) {
	home := t.TempDir()
	zd := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZDOTDIR", zd)
	t.Setenv("XDG_CONFIG_HOME", "")

	file, _, err := InstallHook(Zsh)
	if err != nil {
		t.Fatalf("InstallHook: %v", err)
	}
	if want := filepath.Join(zd, ".zshrc"); file != want {
		t.Fatalf("file = %q, want %q", file, want)
	}
}

func TestHookUnsupportedShell(t *testing.T) {
	setupEnv(t)
	if _, _, err := InstallHook(Shell("ksh")); err == nil {
		t.Fatal("InstallHook for an unsupported shell should fail")
	}
	if _, _, err := RemoveHook(Shell("ksh")); err == nil {
		t.Fatal("RemoveHook for an unsupported shell should fail")
	}
}

func TestHookMalformedBlock(t *testing.T) {
	home := setupEnv(t)
	rc := filepath.Join(home, ".bashrc")
	content := "# mine\n" + BlockBegin + "\n# dangling\n"
	if err := os.WriteFile(rc, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := InstallHook(Bash); err == nil {
		t.Fatal("install with a dangling begin marker should fail")
	}
	if _, _, err := RemoveHook(Bash); err == nil {
		t.Fatal("remove with a dangling begin marker should fail")
	}
	if got := readFile(t, rc); got != content {
		t.Fatal("a malformed block must leave the file untouched")
	}
}
