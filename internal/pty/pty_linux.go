//go:build linux

package pty

import (
	"errors"
	"fmt"
	"io"

	"golang.org/x/sys/unix"
)

func openPTY() (int, int, error) {
	master, err := openPTYMaster()
	if err != nil {
		return -1, -1, err
	}
	if err := unix.IoctlSetPointerInt(master, unix.TIOCSPTLCK, 0); err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("unlock pty: %w", err), closeErr)
	}

	flags := unix.O_RDWR | unix.O_NOCTTY | unix.O_CLOEXEC
	slave, _, errno := unix.Syscall(
		unix.SYS_IOCTL,
		uintptr(master),
		uintptr(unix.TIOCGPTPEER),
		uintptr(flags),
	)
	if errno == 0 {
		return master, int(slave), nil
	}
	if !errors.Is(errno, unix.ENOTTY) && !errors.Is(errno, unix.EINVAL) {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("open pty peer: %w", errno), closeErr)
	}

	// TIOCGPTPEER is unavailable on older kernels. The kernel-provided PTY
	// number is the only path component used by this compatibility path.
	number, err := unix.IoctlGetInt(master, unix.TIOCGPTN)
	if err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("query pty number: %w", err), closeErr)
	}
	slavePath := fmt.Sprintf("/dev/pts/%d", number)
	slaveFD, err := unix.Open(
		slavePath,
		flags|unix.O_NOFOLLOW,
		0,
	)
	if err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("open kernel-named pty slave: %w", err), closeErr)
	}
	if err := requireCharacterDevice(slaveFD); err != nil {
		closeErr := errors.Join(unix.Close(slaveFD), unix.Close(master))
		return -1, -1, errors.Join(err, closeErr)
	}
	return master, slaveFD, nil
}

func openPTYMaster() (int, error) {
	flags := unix.O_RDWR | unix.O_NOCTTY | unix.O_CLOEXEC | unix.O_NOFOLLOW
	master, err := unix.Open("/dev/pts/ptmx", flags, 0)
	if err == nil {
		return master, nil
	}
	if !errors.Is(err, unix.ENOENT) {
		return -1, fmt.Errorf("open pty multiplexer: %w", err)
	}
	master, err = unix.Open("/dev/ptmx", flags, 0)
	if err != nil {
		return -1, fmt.Errorf("open pty multiplexer: %w", err)
	}
	return master, nil
}

func openSocketPair() ([2]int, error) {
	return unix.Socketpair(
		unix.AF_UNIX,
		unix.SOCK_STREAM|unix.SOCK_NONBLOCK|unix.SOCK_CLOEXEC,
		0,
	)
}

func terminalEOF(fd int) (byte, error) {
	terminal, err := unix.IoctlGetTermios(fd, unix.TCGETS)
	if err != nil {
		return 0, err
	}
	return terminal.Cc[unix.VEOF], nil
}

func normalizeMasterReadError(err error) error {
	if errors.Is(err, unix.EIO) {
		return io.EOF
	}
	return err
}

func requireCharacterDevice(fd int) error {
	var status unix.Stat_t
	if err := unix.Fstat(fd, &status); err != nil {
		return fmt.Errorf("inspect pty descriptor: %w", err)
	}
	if status.Mode&unix.S_IFMT != unix.S_IFCHR {
		return errors.New("pty descriptor is not a character device")
	}
	return nil
}
