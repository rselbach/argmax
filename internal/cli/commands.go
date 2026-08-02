package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"syscall"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/shell"
	"github.com/rselbach/argmax/internal/updater"
)

// binaryName is the installed executable name.
const binaryName = "argmax"

// reloadSignal asks the active parent session to replace itself (PRD 9.14).
const reloadSignal = syscall.SIGUSR1

// cmdInit prints the sourceable integration script for one shell (SH-001).
func cmdInit(args []string) int {
	if len(args) != 1 {
		return usageError("usage: argmax init <bash|zsh|fish>")
	}
	sh := shell.Shell(args[0])
	if !sh.Valid() {
		fmt.Fprintf(os.Stderr, "argmax init: unsupported shell %q: supported shells are bash, zsh, fish\n", args[0])
		return 2
	}
	fmt.Print(sh.InitScript())
	return 0
}

// cmdSetup idempotently installs the user-local binary, the shell autostart
// block, and the default configuration (PRD 8.1, 9.14). An unsupported shell
// leaves every file untouched.
func cmdSetup(args []string) int {
	if len(args) > 1 {
		return usageError("usage: argmax setup [bash|zsh|fish]")
	}
	paths := config.ResolvePaths()

	// Resolve the target shell before anything touches the filesystem.
	var sh shell.Shell
	if len(args) == 1 {
		sh = shell.Shell(args[0])
		if !sh.Valid() {
			fmt.Fprintf(os.Stderr, "argmax setup: unsupported shell %q: supported shells are bash, zsh, fish\n", args[0])
			return 2
		}
	} else {
		cfg, err := config.Load(paths.ConfigFile)
		if err != nil {
			return commandError("setup", err)
		}
		sh, err = shell.Detect("", cfg.Core.Shell)
		if err != nil {
			return commandError("setup", err)
		}
	}

	if err := paths.EnsureDirs(); err != nil {
		return commandError("setup", err)
	}

	changed := false

	target, err := localBinary()
	if err != nil {
		return commandError("setup", err)
	}
	binChanged, err := installBinary(target, os.Stdout)
	if err != nil {
		return commandError("setup", err)
	}
	changed = changed || binChanged

	hookFile, hookChanged, err := shell.InstallHook(sh)
	if err != nil {
		return commandError("setup", err)
	}
	if hookChanged {
		fmt.Printf("shell hook: installed argmax block in %s\n", hookFile)
	} else {
		fmt.Printf("shell hook: already installed in %s\n", hookFile)
	}
	changed = changed || hookChanged

	cfgCreated, err := ensureConfigFile(paths.ConfigFile)
	if err != nil {
		return commandError("setup", err)
	}
	if cfgCreated {
		fmt.Printf("config: created %s\n", paths.ConfigFile)
	} else {
		fmt.Printf("config: already exists (%s)\n", paths.ConfigFile)
	}
	changed = changed || cfgCreated

	fmt.Println()
	if changed {
		fmt.Println("setup complete.")
	} else {
		fmt.Println("argmax is already set up; nothing changed.")
	}
	fmt.Printf("to activate, restart your terminal or run: source %s\n", sh.RCFile())
	return 0
}

// cmdConfig implements `argmax config init` and `argmax config show`.
func cmdConfig(args []string) int {
	if len(args) != 1 {
		return usageError("usage: argmax config <init|show>")
	}
	paths := config.ResolvePaths()
	switch args[0] {
	case "init":
		// CFG-002: create the fully commented default only when absent.
		if err := paths.EnsureDirs(); err != nil {
			return commandError("config init", err)
		}
		created, err := ensureConfigFile(paths.ConfigFile)
		if err != nil {
			return commandError("config init", err)
		}
		if created {
			fmt.Printf("created %s\n", paths.ConfigFile)
		} else {
			fmt.Printf("already exists: %s\n", paths.ConfigFile)
		}
		return 0
	case "show":
		// CFG-003: resolved defaults + file + env overrides, redacted.
		cfg, err := config.Load(paths.ConfigFile)
		if err != nil {
			return commandError("config show", err)
		}
		resolved, err := cfg.Resolved()
		if err != nil {
			return commandError("config show", err)
		}
		fmt.Print(resolved)
		return 0
	default:
		return usageError("usage: argmax config <init|show>")
	}
}

// cmdReload validates the configuration and signals the active parent
// session to replace itself (PRD 9.14). Config-only changes also apply via
// the session's own file watcher, so reload succeeds without a session.
func cmdReload(args []string) int {
	if len(args) != 0 {
		return usageError("usage: argmax reload")
	}
	paths := config.ResolvePaths()
	if _, err := config.LoadValid(paths.ConfigFile); err != nil {
		return commandError("reload", err)
	}

	signaled := 0
	for _, file := range sessionFiles(paths.CacheDir) {
		pid, ok := readSessionPID(file)
		if !ok || !processAlive(pid) {
			continue
		}
		if err := syscall.Kill(pid, reloadSignal); err != nil {
			fmt.Fprintf(os.Stderr, "argmax reload: signal pid %d: %v\n", pid, err)
			continue
		}
		fmt.Printf("reload signaled to session (pid %d)\n", pid)
		signaled++
	}
	if signaled == 0 {
		fmt.Println("no active session")
	}
	return 0
}

// sessionFiles lists the transient session markers left by live wrappers.
func sessionFiles(cacheDir string) []string {
	matches, _ := filepath.Glob(filepath.Join(cacheDir, "session-*.json"))
	return matches
}

// readSessionPID parses a session marker's {"pid":..., "ppid":...} payload.
func readSessionPID(path string) (int, bool) {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, false
	}
	var entry struct {
		PID  int `json:"pid"`
		PPID int `json:"ppid"`
	}
	if err := json.Unmarshal(data, &entry); err != nil || entry.PID <= 0 {
		return 0, false
	}
	return entry.PID, true
}

// processAlive reports whether pid exists (EPERM still means alive).
func processAlive(pid int) bool {
	err := syscall.Kill(pid, 0)
	return err == nil || errors.Is(err, syscall.EPERM)
}

// cmdUpdate checks the configured channel and installs a newer verified
// release (UPD-007).
func cmdUpdate(args []string, version string) int {
	if len(args) != 0 {
		return usageError("usage: argmax update")
	}
	paths := config.ResolvePaths()
	cfg, err := config.LoadValid(paths.ConfigFile)
	if err != nil {
		return commandError("update", err)
	}
	if err := updater.SelfUpdate(context.Background(), cfg.Updater.Channel, version, os.Stdout); err != nil {
		return commandError("update", err)
	}
	return 0
}

// cmdCrashLog prints the newest crash report, or removes them with --clear
// (PRD 9.19).
func cmdCrashLog(args []string) int {
	paths := config.ResolvePaths()
	switch {
	case len(args) == 0:
		newest := logs.NewestCrash(paths.CrashesDir)
		if newest == "" {
			fmt.Println("no crash reports")
		} else {
			fmt.Println(newest)
		}
		return 0
	case len(args) == 1 && args[0] == "--clear":
		n, err := logs.ClearCrashes(paths.CrashesDir)
		if err != nil {
			return commandError("crash-log", err)
		}
		fmt.Printf("removed %d crash report(s)\n", n)
		return 0
	default:
		return usageError("usage: argmax crash-log [--clear]")
	}
}

// cmdUninstall removes managed shell hooks, argmax-owned state/config/cache
// trees, and the user-local binary (UN-001..005).
func cmdUninstall(args []string) int {
	if len(args) != 0 {
		return usageError("usage: argmax uninstall")
	}
	paths := config.ResolvePaths()
	failed := false

	// UN-004: warn when running inside a wrapped session.
	if pid := os.Getenv("ARGMAX_SESSION"); pid != "" {
		fmt.Fprintf(os.Stderr, "warning: uninstalling from inside an active argmax session (pid %s): do not kill the parent argmax process; close and reopen the terminal when uninstall finishes\n", pid)
	}

	// UN-001: remove only the managed block from each shell file.
	for _, sh := range []shell.Shell{shell.Bash, shell.Zsh, shell.Fish} {
		file, changed, err := shell.RemoveHook(sh)
		switch {
		case err != nil:
			fmt.Fprintf(os.Stderr, "argmax uninstall: %v\n", err)
			failed = true
		case changed:
			fmt.Printf("removed argmax block from %s\n", file)
		default:
			fmt.Printf("%s: no argmax block\n", file)
		}
	}

	// UN-002: drop the argmax-owned configuration, state, and cache trees.
	for _, dir := range []string{paths.CacheDir, paths.DataDir, filepath.Dir(paths.ConfigFile)} {
		if err := removeTree(dir); err != nil {
			fmt.Fprintf(os.Stderr, "argmax uninstall: %v\n", err)
			failed = true
		}
	}

	// UN-003: remove the user-local binary when present.
	target, err := localBinary()
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax uninstall: %v\n", err)
		failed = true
	} else if _, err := os.Stat(target); err == nil {
		if err := os.Remove(target); err != nil {
			fmt.Fprintf(os.Stderr, "argmax uninstall: remove %s: %v — remove it manually\n", target, err)
			failed = true
		} else {
			fmt.Printf("removed %s\n", target)
		}
	} else {
		fmt.Printf("%s: not present\n", target)
	}

	// UN-005: point at a recognizable running binary left behind elsewhere.
	if exe, err := os.Executable(); err == nil && filepath.Base(exe) == binaryName {
		if target, err := localBinary(); err == nil && !sameFile(exe, target) {
			fmt.Printf("note: the running binary at %s was not removed; remove it manually\n", exe)
		}
	}

	if failed {
		return 1
	}
	fmt.Println("uninstall complete")
	return 0
}

// localBinary is the user-local install location (PRD 8.1, UN-003).
func localBinary() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolve home directory: %w", err)
	}
	return filepath.Join(home, ".local", "bin", binaryName), nil
}

// installBinary copies the running binary to target when needed: it skips
// the copy when the running executable already IS target or when target
// already holds identical bytes. The write is atomic (temp + rename, mode
// 0755). It reports what it did on out.
func installBinary(target string, out io.Writer) (bool, error) {
	exe, err := os.Executable()
	if err != nil {
		return false, fmt.Errorf("locate running binary: %w", err)
	}
	if sameFile(exe, target) {
		_, _ = fmt.Fprintf(out, "binary: already running from %s\n", target)
		return false, nil
	}
	src, err := os.ReadFile(exe)
	if err != nil {
		return false, fmt.Errorf("read %s: %w", exe, err)
	}
	if existing, err := os.ReadFile(target); err == nil && bytes.Equal(existing, src) {
		_, _ = fmt.Fprintf(out, "binary: already up to date (%s)\n", target)
		return false, nil
	}

	dir := filepath.Dir(target)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return false, fmt.Errorf("create %s: %w", dir, err)
	}
	tmp, err := os.CreateTemp(dir, ".argmax-*")
	if err != nil {
		return false, fmt.Errorf("create temp file in %s: %w", dir, err)
	}
	tmpName := tmp.Name()
	defer func() { _ = os.Remove(tmpName) }()
	if _, err := tmp.Write(src); err != nil {
		_ = tmp.Close()
		return false, fmt.Errorf("write %s: %w", tmpName, err)
	}
	if err := tmp.Chmod(0o755); err != nil {
		_ = tmp.Close()
		return false, fmt.Errorf("chmod %s: %w", tmpName, err)
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		return false, fmt.Errorf("sync %s: %w", tmpName, err)
	}
	if err := tmp.Close(); err != nil {
		return false, fmt.Errorf("close %s: %w", tmpName, err)
	}
	if err := os.Rename(tmpName, target); err != nil {
		return false, fmt.Errorf("install %s: %w", target, err)
	}
	_, _ = fmt.Fprintf(out, "binary: installed to %s\n", target)
	return true, nil
}

// sameFile reports whether a and b resolve to the same filesystem path.
func sameFile(a, b string) bool {
	resolve := func(p string) string {
		if abs, err := filepath.Abs(p); err == nil {
			p = abs
		}
		if real, err := filepath.EvalSymlinks(p); err == nil {
			p = real
		}
		return p
	}
	return resolve(a) == resolve(b)
}

// ensureConfigFile creates the commented default configuration at path when
// absent (CFG-002), reporting whether it created the file. An existing file
// is never touched.
func ensureConfigFile(path string) (bool, error) {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if errors.Is(err, fs.ErrExist) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("create %s: %w", path, err)
	}
	if _, err := f.WriteString(config.DefaultFile); err != nil {
		_ = f.Close()
		return false, fmt.Errorf("write %s: %w", path, err)
	}
	if err := f.Close(); err != nil {
		return false, fmt.Errorf("write %s: %w", path, err)
	}
	return true, nil
}

// removeTree deletes an argmax-owned directory, reporting the removal or the
// exact manual action on failure (UN-002, UN-005).
func removeTree(dir string) error {
	if _, err := os.Stat(dir); err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			fmt.Printf("%s: not present\n", dir)
			return nil
		}
		return fmt.Errorf("stat %s: %w", dir, err)
	}
	if err := os.RemoveAll(dir); err != nil {
		return fmt.Errorf("remove %s: %v — remove it manually: rm -rf %s", dir, err, dir)
	}
	fmt.Printf("removed %s\n", dir)
	return nil
}
