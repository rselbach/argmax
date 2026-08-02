package shell

import (
	"os"
	"path/filepath"
)

// homeDir returns the user's home directory, or "" when it cannot be
// resolved.
func homeDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return home
}

// zdotdir returns zsh's effective configuration directory: $ZDOTDIR when
// set, otherwise $HOME.
func zdotdir() string {
	if zd := os.Getenv("ZDOTDIR"); zd != "" {
		return zd
	}
	return homeDir()
}

// fishConfigFile resolves fish's XDG-aware config file.
func fishConfigFile() string {
	if xdg := os.Getenv("XDG_CONFIG_HOME"); xdg != "" {
		return filepath.Join(xdg, "fish", "config.fish")
	}
	return filepath.Join(homeDir(), ".config", "fish", "config.fish")
}

// AliasFiles returns the alias-bearing configuration files for the shell in
// discovery order (SH-007).
func (s Shell) AliasFiles() []string {
	switch s {
	case Bash:
		home := homeDir()
		return []string{
			filepath.Join(home, ".bashrc"),
			filepath.Join(home, ".bash_profile"),
			filepath.Join(home, ".bash_aliases"),
		}
	case Zsh:
		zd := zdotdir()
		return []string{
			filepath.Join(zd, ".zshrc"),
			filepath.Join(zd, ".zprofile"),
			filepath.Join(zd, ".zshenv"),
		}
	case Fish:
		return []string{fishConfigFile()}
	}
	return nil
}

// HistoryPath resolves the shell's history file (HIST-002): $HISTFILE when
// set, otherwise the shell's XDG-aware/default path. getenv may be nil, in
// which case os.Getenv is used.
func (s Shell) HistoryPath(getenv func(string) string) string {
	if getenv == nil {
		getenv = os.Getenv
	}
	if h := getenv("HISTFILE"); h != "" {
		return h
	}
	home := getenv("HOME")
	switch s {
	case Bash:
		return filepath.Join(home, ".bash_history")
	case Zsh:
		zd := getenv("ZDOTDIR")
		if zd == "" {
			zd = home
		}
		return filepath.Join(zd, ".zsh_history")
	case Fish:
		if xdg := getenv("XDG_DATA_HOME"); xdg != "" {
			return filepath.Join(xdg, "fish", "fish_history")
		}
		return filepath.Join(home, ".local", "share", "fish", "fish_history")
	}
	return ""
}

// RCFile is the primary file that receives the managed setup block. For Bash
// this is always ~/.bashrc (even on macOS, where login shells read
// .bash_profile instead — `argmax setup` reports the exact file so users can
// source it from .bash_profile if they use login shells).
func (s Shell) RCFile() string {
	switch s {
	case Bash:
		return filepath.Join(homeDir(), ".bashrc")
	case Zsh:
		return filepath.Join(zdotdir(), ".zshrc")
	case Fish:
		return fishConfigFile()
	}
	return ""
}
