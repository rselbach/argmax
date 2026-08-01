//go:build darwin

package pty

import (
	"bytes"
	"errors"
	"fmt"
	"path/filepath"
	"runtime"
	"syscall"
	"unsafe"

	"golang.org/x/sys/unix"
)

const darwinPTYNameBytes = 128

func openPTY() (int, int, error) {
	flags := unix.O_RDWR | unix.O_NOCTTY | unix.O_CLOEXEC | unix.O_NOFOLLOW
	master, err := unix.Open("/dev/ptmx", flags, 0)
	if err != nil {
		return -1, -1, fmt.Errorf("open pty multiplexer: %w", err)
	}
	if err := unix.IoctlSetInt(master, unix.TIOCPTYGRANT, 0); err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("grant pty: %w", err), closeErr)
	}

	nameBuffer := make([]byte, darwinPTYNameBytes)
	if err := darwinIoctlPointer(master, unix.TIOCPTYGNAME, &nameBuffer[0]); err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("query pty name: %w", err), closeErr)
	}
	terminator := bytes.IndexByte(nameBuffer, 0)
	if terminator < 0 {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(errors.New("kernel pty name is unterminated"), closeErr)
	}
	name := string(nameBuffer[:terminator])
	if !filepath.IsAbs(name) || filepath.Clean(name) != name || filepath.Dir(name) != "/dev" {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(errors.New("kernel pty name is invalid"), closeErr)
	}

	if err := unix.IoctlSetInt(master, unix.TIOCPTYUNLK, 0); err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("unlock pty: %w", err), closeErr)
	}
	slave, err := unix.Open(name, flags, 0)
	if err != nil {
		closeErr := unix.Close(master)
		return -1, -1, errors.Join(fmt.Errorf("open kernel-named pty slave: %w", err), closeErr)
	}
	if err := requireCharacterDevice(slave); err != nil {
		closeErr := errors.Join(unix.Close(slave), unix.Close(master))
		return -1, -1, errors.Join(err, closeErr)
	}
	return master, slave, nil
}

func openSocketPair() ([2]int, error) {
	// Darwin has no SOCK_CLOEXEC flag. ForkLock closes the only inheritance
	// window until both descriptors have FD_CLOEXEC and O_NONBLOCK.
	syscall.ForkLock.Lock()
	defer syscall.ForkLock.Unlock()

	pair, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_STREAM, 0)
	if err != nil {
		return [2]int{}, err
	}
	for _, fd := range pair {
		flags, flagErr := unix.FcntlInt(uintptr(fd), unix.F_GETFD, 0)
		if flagErr == nil {
			_, flagErr = unix.FcntlInt(uintptr(fd), unix.F_SETFD, flags|unix.FD_CLOEXEC)
		}
		if flagErr == nil {
			flagErr = unix.SetNonblock(fd, true)
		}
		if flagErr != nil {
			closeErr := errors.Join(unix.Close(pair[0]), unix.Close(pair[1]))
			return [2]int{}, errors.Join(flagErr, closeErr)
		}
	}
	return pair, nil
}

func terminalEOF(fd int) (byte, error) {
	terminal, err := unix.IoctlGetTermios(fd, unix.TIOCGETA)
	if err != nil {
		return 0, err
	}
	return terminal.Cc[unix.VEOF], nil
}

func normalizeMasterReadError(err error) error { return err }

func darwinIoctlPointer(fd int, request uint, value *byte) error {
	_, _, errno := unix.Syscall9(
		// x/sys has no typed ioctl wrapper for Darwin's 128-byte TIOCPTYGNAME.
		unix.SYS_IOCTL, //nolint:staticcheck // a raw pointer ioctl is required
		uintptr(fd),
		uintptr(request),
		uintptr(unsafe.Pointer(value)),
		0, 0, 0, 0, 0, 0,
	)
	runtime.KeepAlive(value)
	if errno != 0 {
		return errno
	}
	return nil
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
