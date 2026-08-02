//go:build darwin

package pty

import (
	"errors"

	"golang.org/x/sys/unix"
)

// Darwin reports EPERM when a zombie is the only remaining process-group
// member. The child remains pinned and waitable; this is not a failed attempt
// to contain a live descendant.
func ignorableProcessGroupError(err error) bool {
	return errors.Is(err, unix.ESRCH) || errors.Is(err, unix.EPERM)
}
