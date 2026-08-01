//go:build linux

package terminal

import (
	"errors"
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

func openTestPTY() (*os.File, *os.File, error) {
	flags := unix.O_RDWR | unix.O_NOCTTY | unix.O_CLOEXEC
	master, err := unix.Open("/dev/ptmx", flags, 0)
	if err != nil {
		return nil, nil, err
	}
	if err := unix.IoctlSetPointerInt(master, unix.TIOCSPTLCK, 0); err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	number, err := unix.IoctlGetInt(master, unix.TIOCGPTN)
	if err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	slave, err := unix.Open(fmt.Sprintf("/dev/pts/%d", number), flags|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	return os.NewFile(uintptr(master), "test-pty-master"),
		os.NewFile(uintptr(slave), "test-pty-slave"), nil
}
