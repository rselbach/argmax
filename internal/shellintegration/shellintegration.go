// Package shellintegration provides sourceable integration scripts for the
// shells supported by the transparent runtime.
package shellintegration

import (
	_ "embed"
	"errors"

	"github.com/rselbach/argmax/internal/shellselect"
)

const (
	// SessionMarkerEnvironment marks a shell owned by an argmax session.
	SessionMarkerEnvironment = "ARGMAX_PRIVATE_SESSION"
	// SessionOwnerPIDEnvironment binds a private session to its owning shell.
	SessionOwnerPIDEnvironment = "ARGMAX_SESSION_OWNER_PID"

	// MaxSyncEventCharacters is the maximum editing-buffer character count.
	MaxSyncEventCharacters = 16 * 1024
	// MaxSyncEventFrameCharacters includes protocol framing headroom.
	MaxSyncEventFrameCharacters = MaxSyncEventCharacters + 33
	// MaxSyncEventWireBytes conservatively allows four UTF-8 bytes per character
	// and the terminating NUL.
	MaxSyncEventWireBytes = MaxSyncEventFrameCharacters*4 + 1

	// SyncProbeSequence requests a Bash or Zsh editing-buffer snapshot.
	SyncProbeSequence = "\x1b[argmax-sync~"
	// FishSyncProbeSequence requests a Fish editing-buffer snapshot.
	FishSyncProbeSequence = "\x1e"
)

// ErrUnsupportedShell reports a shell kind without an integration adapter.
var ErrUnsupportedShell = errors.New("shell integration requires bash, zsh, or fish")

//go:embed scripts/bash.sh
var bashScript string

//go:embed scripts/zsh.sh
var zshScript string

//go:embed scripts/fish.fish
var fishScript string

// Script returns LF-terminated sourceable integration code with no
// human-oriented output.
func Script(shell shellselect.Kind) (string, error) {
	switch shell {
	case shellselect.Bash:
		return bashScript, nil
	case shellselect.Zsh:
		return zshScript, nil
	case shellselect.Fish:
		return fishScript, nil
	default:
		return "", ErrUnsupportedShell
	}
}
