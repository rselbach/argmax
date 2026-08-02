//go:build linux

package pty

import (
	"errors"

	"golang.org/x/sys/unix"
)

func ignorableProcessGroupError(err error) bool {
	return errors.Is(err, unix.ESRCH)
}
