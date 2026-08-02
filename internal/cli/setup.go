package cli

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/paths"
	"github.com/rselbach/argmax/internal/shell"
)

// cmdSetup idempotently installs the binary when needed, configures shell
// autostart, and initializes configuration. An unsupported shell leaves
// all shell files untouched.
func cmdSetup(args []string) int {
	var target shell.Kind
	switch {
	case len(args) == 0:
		detected, err := shell.Detect("", "")
		if err != nil {
			fmt.Fprintln(os.Stderr, "argmax:", err)
			return 1
		}
		target = detected
	case shell.Supported(args[0]):
		target = shell.Kind(args[0])
	default:
		fmt.Fprintf(os.Stderr, "argmax: unsupported shell %q; supported shells are bash, zsh, and fish\n", args[0])
		return 1
	}

	if path, copied, err := ensureBinaryOnPath(); err != nil {
		fmt.Fprintln(os.Stderr, "argmax: install binary:", err)
		return 1
	} else if copied {
		fmt.Printf("installed binary to %s\n", path)
	}

	rc := target.RCFile()
	if rc == "" {
		fmt.Fprintln(os.Stderr, "argmax: cannot resolve the shell configuration file")
		return 1
	}
	changed, err := installBlock(rc, shell.Block(target))
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	if changed {
		fmt.Printf("added argmax autostart to %s\n", rc)
	} else {
		fmt.Printf("autostart already present in %s\n", rc)
	}

	if cmdConfigInit() != 0 {
		return 1
	}
	fmt.Printf("\nsetup complete for %s\nactivate now with: source %q (or restart the terminal)\n", target, rc)
	return 0
}

// ensureBinaryOnPath copies the running binary to the user-local bin
// directory when argmax is not already reachable on PATH.
func ensureBinaryOnPath() (string, bool, error) {
	self, err := os.Executable()
	if err != nil {
		return "", false, err
	}
	for _, dir := range filepath.SplitList(os.Getenv("PATH")) {
		candidate := filepath.Join(dir, "argmax")
		if info, err := os.Stat(candidate); err == nil && info.Mode().IsRegular() {
			return candidate, false, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", false, err
	}
	binDir := filepath.Join(home, ".local", "bin")
	if err := os.MkdirAll(binDir, 0o755); err != nil {
		return "", false, err
	}
	dest := filepath.Join(binDir, "argmax")
	if err := copyFile(self, dest); err != nil {
		return "", false, err
	}
	fmt.Printf("note: ensure %s is on your PATH\n", binDir)
	return dest, true, nil
}

func copyFile(src, dest string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer func() { _ = in.Close() }()
	out, err := os.OpenFile(dest+".tmp", os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o755)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		_ = out.Close()
		_ = os.Remove(dest + ".tmp")
		return err
	}
	if err := out.Close(); err != nil {
		_ = os.Remove(dest + ".tmp")
		return err
	}
	return os.Rename(dest+".tmp", dest)
}

// installBlock appends the marked autostart block when absent. Returns
// whether the file changed.
func installBlock(rc, block string) (bool, error) {
	data, err := os.ReadFile(rc)
	if err != nil && !os.IsNotExist(err) {
		return false, fmt.Errorf("read %s: %w", rc, err)
	}
	content := string(data)
	if strings.Contains(content, shell.BeginMarker) {
		return false, nil
	}
	if err := os.MkdirAll(filepath.Dir(rc), 0o755); err != nil {
		return false, fmt.Errorf("create %s: %w", filepath.Dir(rc), err)
	}
	var b strings.Builder
	b.WriteString(content)
	if content != "" && !strings.HasSuffix(content, "\n") {
		b.WriteString("\n")
	}
	if content != "" {
		b.WriteString("\n")
	}
	b.WriteString(block)
	b.WriteString("\n")
	perm := os.FileMode(0o644)
	if info, err := os.Stat(rc); err == nil {
		perm = info.Mode().Perm()
	}
	if err := os.WriteFile(rc, []byte(b.String()), perm); err != nil {
		return false, fmt.Errorf("write %s: %w", rc, err)
	}
	return true, nil
}

// cmdUninstall removes managed hooks, product state, and identifiable
// binaries, reporting every removed location.
func cmdUninstall() int {
	if os.Getenv("ARGMAX_ACTIVE") != "" {
		fmt.Println("warning: you are inside an active argmax session.")
		fmt.Println("do not kill the parent process; close and reopen the terminal when done.")
	}
	failures := 0
	for _, k := range []shell.Kind{shell.Bash, shell.Zsh, shell.Fish} {
		rc := k.RCFile()
		if rc == "" {
			continue
		}
		removed, err := removeBlock(rc)
		switch {
		case err != nil:
			fmt.Fprintf(os.Stderr, "argmax: %s: %v\n", rc, err)
			failures++
		case removed:
			fmt.Printf("removed argmax block from %s\n", rc)
		}
	}
	for _, dir := range []string{
		filepath.Dir(config.Path()),
		paths.DataDir(),
		paths.CacheDir(),
	} {
		if _, err := os.Stat(dir); err != nil {
			continue
		}
		if err := os.RemoveAll(dir); err != nil {
			fmt.Fprintf(os.Stderr, "argmax: remove %s: %v (remove manually)\n", dir, err)
			failures++
			continue
		}
		fmt.Printf("removed %s\n", dir)
	}
	removeBinaries(&failures)
	if failures > 0 {
		fmt.Println("some artifacts require manual removal; see messages above")
		return 1
	}
	fmt.Println("argmax was uninstalled")
	return 0
}

// removeBlock deletes only the marked argmax block, preserving
// permissions and unrelated content.
func removeBlock(rc string) (bool, error) {
	data, err := os.ReadFile(rc)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	content := string(data)
	begin := strings.Index(content, shell.BeginMarker)
	if begin < 0 {
		return false, nil
	}
	end := strings.Index(content, shell.EndMarker)
	if end < 0 {
		return false, fmt.Errorf("found begin marker without end marker; not modifying the file")
	}
	end += len(shell.EndMarker)
	// Swallow one trailing newline and preceding blank line.
	if end < len(content) && content[end] == '\n' {
		end++
	}
	head := strings.TrimRight(content[:begin], "\n")
	if head != "" {
		head += "\n"
	}
	tail := content[end:]
	perm := os.FileMode(0o644)
	if info, err := os.Stat(rc); err == nil {
		perm = info.Mode().Perm()
	}
	if err := os.WriteFile(rc, []byte(head+tail), perm); err != nil {
		return false, err
	}
	return true, nil
}

func removeBinaries(failures *int) {
	home, err := os.UserHomeDir()
	if err != nil {
		return
	}
	self, _ := os.Executable()
	for _, candidate := range []string{
		filepath.Join(home, ".local", "bin", "argmax"),
		"/usr/local/bin/argmax",
	} {
		info, err := os.Stat(candidate)
		if err != nil || !info.Mode().IsRegular() {
			continue
		}
		if self != "" && candidate == self {
			// Removing the running binary is safe on POSIX.
			fmt.Printf("removing the running binary %s\n", candidate)
		}
		if err := os.Remove(candidate); err != nil {
			fmt.Fprintf(os.Stderr, "argmax: remove %s: %v (remove manually, e.g. with sudo)\n", candidate, err)
			*failures++
			continue
		}
		fmt.Printf("removed %s\n", candidate)
	}
}
