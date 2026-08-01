//go:build darwin

package terminal

import (
	"bytes"
	"errors"
	"os"
	"runtime"
	"unsafe"

	"golang.org/x/sys/unix"
)

func openTestPTY() (*os.File, *os.File, error) {
	flags := unix.O_RDWR | unix.O_NOCTTY | unix.O_CLOEXEC | unix.O_NOFOLLOW
	master, err := unix.Open("/dev/ptmx", flags, 0)
	if err != nil {
		return nil, nil, err
	}
	if err := unix.IoctlSetInt(master, unix.TIOCPTYGRANT, 0); err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	name := make([]byte, 128)
	if err := testDarwinIoctlPointer(master, unix.TIOCPTYGNAME, &name[0]); err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	end := bytes.IndexByte(name, 0)
	if end < 0 {
		return nil, nil, errors.Join(errors.New("unterminated test pty name"), unix.Close(master))
	}
	if err := unix.IoctlSetInt(master, unix.TIOCPTYUNLK, 0); err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	slave, err := unix.Open(string(name[:end]), flags, 0)
	if err != nil {
		return nil, nil, errors.Join(err, unix.Close(master))
	}
	return os.NewFile(uintptr(master), "test-pty-master"),
		os.NewFile(uintptr(slave), "test-pty-slave"), nil
}

func testDarwinIoctlPointer(fd int, request uint, value *byte) error {
	_, _, errno := unix.Syscall9(
		unix.SYS_IOCTL, //nolint:staticcheck // Darwin has no typed wrapper.
		uintptr(fd), uintptr(request), uintptr(unsafe.Pointer(value)),
		0, 0, 0, 0, 0, 0,
	)
	runtime.KeepAlive(value)
	if errno != 0 {
		return errno
	}
	return nil
}
