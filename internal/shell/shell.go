// Package shell knows the supported shells: detection, executable paths,
// integration hooks, alias sources, and history locations for Bash, Zsh,
// and Fish.
package shell

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
)

// Kind identifies a supported shell.
type Kind string

// Supported shells.
const (
	Bash Kind = "bash"
	Zsh  Kind = "zsh"
	Fish Kind = "fish"
)

// Supported reports whether name is a supported shell.
func Supported(name string) bool {
	switch Kind(name) {
	case Bash, Zsh, Fish:
		return true
	}
	return false
}

// Detect resolves the shell to wrap using the documented precedence:
// explicit CLI flag, resolved configuration, autostart hook marker,
// parent-process inspection, $SHELL, then Bash.
func Detect(flag, configured string) (Kind, error) {
	if flag != "" {
		if !Supported(flag) {
			return "", fmt.Errorf("unsupported shell %q: supported shells are bash, zsh, and fish", flag)
		}
		return Kind(flag), nil
	}
	if configured != "" {
		if !Supported(configured) {
			return "", fmt.Errorf("unsupported configured shell %q: supported shells are bash, zsh, and fish", configured)
		}
		return Kind(configured), nil
	}
	if marker := os.Getenv("ARGMAX_SHELL"); Supported(marker) {
		return Kind(marker), nil
	}
	if k, ok := parentShell(); ok {
		return k, nil
	}
	if base := filepath.Base(os.Getenv("SHELL")); Supported(base) {
		return Kind(base), nil
	}
	return Bash, nil
}

// parentShell inspects the parent process name where the platform allows.
func parentShell() (Kind, bool) {
	ppid := os.Getppid()
	var name string
	switch runtime.GOOS {
	case "linux":
		data, err := os.ReadFile("/proc/" + strconv.Itoa(ppid) + "/comm")
		if err != nil {
			return "", false
		}
		name = strings.TrimSpace(string(data))
	case "darwin":
		out, err := exec.Command("ps", "-o", "comm=", "-p", strconv.Itoa(ppid)).Output()
		if err != nil {
			return "", false
		}
		name = filepath.Base(strings.TrimSpace(string(out)))
	default:
		return "", false
	}
	name = strings.TrimPrefix(name, "-") // login shells
	if Supported(name) {
		return Kind(name), true
	}
	return "", false
}

// Path resolves the shell executable on PATH.
func (k Kind) Path() (string, error) {
	p, err := exec.LookPath(string(k))
	if err != nil {
		return "", fmt.Errorf("shell %s not found on PATH: %w", k, err)
	}
	return p, nil
}

// HistoryPath returns the shell's history file, honoring $HISTFILE and
// XDG-aware defaults.
func (k Kind) HistoryPath() string {
	if hf := os.Getenv("HISTFILE"); hf != "" {
		return hf
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	switch k {
	case Bash:
		return filepath.Join(home, ".bash_history")
	case Zsh:
		if zdot := os.Getenv("ZDOTDIR"); zdot != "" {
			return filepath.Join(zdot, ".zsh_history")
		}
		return filepath.Join(home, ".zsh_history")
	case Fish:
		if xdg := os.Getenv("XDG_DATA_HOME"); xdg != "" {
			return filepath.Join(xdg, "fish", "fish_history")
		}
		return filepath.Join(home, ".local", "share", "fish", "fish_history")
	}
	return ""
}

// AliasFiles returns the configuration files scanned for alias definitions.
func (k Kind) AliasFiles() []string {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	switch k {
	case Bash:
		return []string{
			filepath.Join(home, ".bashrc"),
			filepath.Join(home, ".bash_profile"),
			filepath.Join(home, ".bash_aliases"),
		}
	case Zsh:
		zdot := os.Getenv("ZDOTDIR")
		if zdot == "" {
			zdot = home
		}
		return []string{
			filepath.Join(zdot, ".zshrc"),
			filepath.Join(zdot, ".zshenv"),
			filepath.Join(zdot, ".zprofile"),
		}
	case Fish:
		cfg := os.Getenv("XDG_CONFIG_HOME")
		if cfg == "" {
			cfg = filepath.Join(home, ".config")
		}
		return []string{filepath.Join(cfg, "fish", "config.fish")}
	}
	return nil
}

// RCFile returns the file that receives the autostart block for setup.
func (k Kind) RCFile() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	switch k {
	case Bash:
		return filepath.Join(home, ".bashrc")
	case Zsh:
		zdot := os.Getenv("ZDOTDIR")
		if zdot == "" {
			zdot = home
		}
		return filepath.Join(zdot, ".zshrc")
	case Fish:
		cfg := os.Getenv("XDG_CONFIG_HOME")
		if cfg == "" {
			cfg = filepath.Join(home, ".config")
		}
		return filepath.Join(cfg, "fish", "config.fish")
	}
	return ""
}
