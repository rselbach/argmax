//go:build linux || darwin

// Package transparuntime runs one byte-transparent interactive shell session.
package transparuntime

import (
	"crypto/rand"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"os"
	osSignal "os/signal"
	"syscall"
	"time"

	"github.com/rselbach/argmax/internal/pty"
	"github.com/rselbach/argmax/internal/shellselect"
	"github.com/rselbach/argmax/internal/terminal"
	"golang.org/x/sys/unix"
)

const (
	queueBytes        = 256 * 1024
	ioBytes           = 64 * 1024
	pollInterval      = 20 * time.Millisecond
	trailingDrainTime = 500 * time.Millisecond
	signalQueueSize   = 32
)

var (
	// ErrRuntimePanic reports a recovered panic after owned resources restored.
	ErrRuntimePanic = errors.New("transparent shell runtime panicked")
	// ErrExternalReap reports loss of the direct child's native status.
	ErrExternalReap = errors.New("transparent shell child was reaped externally")
)

// Config describes one interactive transparent-shell run.
type Config struct {
	Shell  shellselect.Selected
	Cwd    string
	Marker string
	Input  *os.File
	Output *os.File
}

// String redacts descriptors, paths, and the private marker.
func (config Config) String() string {
	return fmt.Sprintf("transparuntime.Config{shell:%s, cwd:<redacted>, marker:<redacted>, input:<redacted>, output:<redacted>}", config.Shell.Kind())
}

// GoString redacts descriptors, paths, and the private marker.
func (config Config) GoString() string { return config.String() }

// GenerateMarker returns a private, shell-environment-safe session marker.
func GenerateMarker() (string, error) {
	bytes := make([]byte, 32)
	if _, err := io.ReadFull(rand.Reader, bytes); err != nil {
		return "", fmt.Errorf("generate private session marker: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(bytes), nil
}

// Run executes one session.
func Run(config Config) (pty.ExitStatus, error) {
	return run(config, nil)
}

// run's recovery defer is registered before resource ownership, so later
// terminal and PTY defers run before panic recovery.
func run(config Config, afterStart func(*pty.Session)) (status pty.ExitStatus, err error) {
	defer func() {
		if recover() != nil {
			status = pty.ExitStatus{}
			err = errors.Join(err, ErrRuntimePanic)
		}
	}()
	if config.Input == nil || config.Output == nil {
		return pty.ExitStatus{}, errors.New("transparent runtime requires terminal input and output")
	}
	dimensions, err := terminal.FromTerminal(config.Input)
	if err != nil {
		return pty.ExitStatus{}, err
	}
	guard, err := terminal.Enter(config.Input, config.Output, config.Output, dimensions)
	if err != nil {
		return pty.ExitStatus{}, err
	}
	defer func() { err = errors.Join(err, guard.Close()) }()

	restoreFlags, err := makeNonblocking(config.Input, config.Output)
	if err != nil {
		return pty.ExitStatus{}, err
	}
	defer func() { err = errors.Join(err, restoreFlags()) }()

	session, err := pty.Start(pty.Config{
		Executable: config.Shell.Executable(),
		Args:       []string{"-i"},
		Cwd:        config.Cwd,
		Size:       dimensions.Size(),
		Marker:     config.Marker,
		ShellKind:  config.Shell.Kind().String(),
	})
	if err != nil {
		return pty.ExitStatus{}, err
	}
	defer func() { err = errors.Join(err, session.Shutdown()) }()
	if afterStart != nil {
		afterStart(session)
	}

	return eventLoop(config, guard, session)
}

type descriptorFlags struct {
	fd    int
	flags int
}

func makeNonblocking(files ...*os.File) (func() error, error) {
	saved := make([]descriptorFlags, 0, len(files))
	seen := make(map[int]struct{}, len(files))
	for _, file := range files {
		fd := int(file.Fd())
		if _, ok := seen[fd]; ok {
			continue
		}
		seen[fd] = struct{}{}
		flags, err := unix.FcntlInt(uintptr(fd), unix.F_GETFL, 0)
		if err != nil {
			return nil, errors.Join(
				errors.New("inspect parent terminal descriptor flags"),
				restoreDescriptorFlags(saved),
			)
		}
		saved = append(saved, descriptorFlags{fd: fd, flags: flags})
		if err := unix.SetNonblock(fd, true); err != nil {
			return nil, errors.Join(
				errors.New("make parent terminal descriptor nonblocking"),
				restoreDescriptorFlags(saved),
			)
		}
	}
	return func() error { return restoreDescriptorFlags(saved) }, nil
}

func restoreDescriptorFlags(saved []descriptorFlags) error {
	var result error
	for index := len(saved) - 1; index >= 0; index-- {
		value := saved[index]
		if _, err := unix.FcntlInt(uintptr(value.fd), unix.F_SETFL, value.flags); err != nil {
			result = errors.Join(result, errors.New("restore parent terminal descriptor flags"))
		}
	}
	return result
}

type byteQueue struct {
	data       []byte
	start, end int
}

func newByteQueue() byteQueue {
	return byteQueue{data: make([]byte, queueBytes)}
}

func (queue *byteQueue) length() int { return queue.end - queue.start }

func (queue *byteQueue) readable() []byte { return queue.data[queue.start:queue.end] }

func (queue *byteQueue) writable() []byte {
	if queue.end == len(queue.data) && queue.start != 0 {
		copy(queue.data, queue.data[queue.start:queue.end])
		queue.end -= queue.start
		queue.start = 0
	}
	return queue.data[queue.end:]
}

func (queue *byteQueue) produced(count int) { queue.end += count }

func (queue *byteQueue) consumed(count int) {
	queue.start += count
	if queue.start == queue.end {
		queue.start = 0
		queue.end = 0
	}
}

func eventLoop(config Config, guard *terminal.Guard, session *pty.Session) (pty.ExitStatus, error) {
	signals := make(chan os.Signal, signalQueueSize)
	osSignal.Notify(
		signals,
		syscall.SIGWINCH, syscall.SIGINT, syscall.SIGQUIT, syscall.SIGTERM,
		syscall.SIGHUP, syscall.SIGTSTP, syscall.SIGCONT, syscall.SIGPIPE,
	)
	defer osSignal.Stop(signals)

	inputQueue := newByteQueue()
	outputQueue := newByteQueue()
	inputEOF := false
	inputClosed := false
	masterEOF := false
	var childExit *pty.ExitStatus
	var drainDeadline time.Time

	for {
		if err := drainSignals(signals, config.Input, guard, session); err != nil {
			return statusOrZero(childExit), err
		}
		if childExit == nil {
			status, err := session.TryWait()
			if err != nil {
				return pty.ExitStatus{}, fmt.Errorf("poll shell status: %w", err)
			}
			if status != nil {
				if status.Kind == pty.ExitExternallyReaped {
					return *status, ErrExternalReap
				}
				childExit = status
				drainDeadline = time.Now().Add(trailingDrainTime)
			}
		}
		if childExit != nil {
			if masterEOF && outputQueue.length() == 0 {
				return *childExit, nil
			}
			if !time.Now().Before(drainDeadline) {
				return *childExit, nil
			}
		}

		pollDescriptors := []unix.PollFd{
			{Fd: int32(config.Input.Fd())},
			{Fd: int32(config.Output.Fd())},
			{Fd: int32(session.Master().FD())},
			{Fd: int32(session.Integration().FD())},
		}
		if !inputEOF && len(inputQueue.writable()) != 0 {
			pollDescriptors[0].Events = unix.POLLIN
		}
		if outputQueue.length() != 0 {
			pollDescriptors[1].Events = unix.POLLOUT
		}
		if !masterEOF && len(outputQueue.writable()) != 0 {
			pollDescriptors[2].Events |= unix.POLLIN
		}
		if inputQueue.length() != 0 || inputEOF && !inputClosed {
			pollDescriptors[2].Events |= unix.POLLOUT
		}
		pollDescriptors[3].Events = unix.POLLIN

		timeout := pollInterval
		if childExit != nil {
			remaining := time.Until(drainDeadline)
			if remaining < timeout {
				timeout = max(remaining, 0)
			}
		}
		_, pollErr := unix.Poll(pollDescriptors, int(timeout.Milliseconds()))
		if pollErr != nil && !errors.Is(pollErr, unix.EINTR) {
			return statusOrZero(childExit), fmt.Errorf("poll transparent shell descriptors: %w", pollErr)
		}

		if err := drainIntegration(session.Integration()); err != nil {
			return statusOrZero(childExit), err
		}
		if outputQueue.length() != 0 && pollDescriptors[1].Revents&(unix.POLLOUT|unix.POLLHUP|unix.POLLERR) != 0 {
			if err := writeParentOutput(config.Output, &outputQueue); err != nil {
				return statusOrZero(childExit), err
			}
		}
		if !masterEOF && pollDescriptors[2].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) != 0 {
			eof, err := readShellOutput(session.Master(), &outputQueue)
			if err != nil {
				return statusOrZero(childExit), err
			}
			masterEOF = eof
		}
		if pollDescriptors[2].Revents&unix.POLLOUT != 0 {
			if inputQueue.length() != 0 {
				if err := writeShellInput(session.Master(), &inputQueue); err != nil {
					return statusOrZero(childExit), err
				}
			}
			if inputQueue.length() == 0 && inputEOF && !inputClosed {
				progress, err := session.CloseInput()
				if err != nil {
					return statusOrZero(childExit), fmt.Errorf("close shell input: %w", err)
				}
				inputClosed = progress.Closed
			}
		}
		if !inputEOF && pollDescriptors[0].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) != 0 {
			eof, err := readParentInput(config.Input, &inputQueue)
			if err != nil {
				return statusOrZero(childExit), err
			}
			inputEOF = eof
		}
	}
}

func statusOrZero(status *pty.ExitStatus) pty.ExitStatus {
	if status == nil {
		return pty.ExitStatus{}
	}
	return *status
}

func drainSignals(signals <-chan os.Signal, input *os.File, guard *terminal.Guard, session *pty.Session) error {
	for range signalQueueSize {
		select {
		case received := <-signals:
			switch received {
			case syscall.SIGWINCH:
				if err := resize(input, guard, session); err != nil {
					return err
				}
			case syscall.SIGTSTP:
				if err := suspend(input, guard, session); err != nil {
					return err
				}
			case syscall.SIGINT:
				if err := forward(session, pty.SignalInterrupt); err != nil {
					return err
				}
			case syscall.SIGQUIT:
				if err := forward(session, pty.SignalQuit); err != nil {
					return err
				}
			case syscall.SIGTERM:
				if err := forward(session, pty.SignalTerminate); err != nil {
					return err
				}
			case syscall.SIGHUP:
				if err := forward(session, pty.SignalHangup); err != nil {
					return err
				}
			case syscall.SIGCONT:
				if err := forward(session, pty.SignalContinue); err != nil {
					return err
				}
			}
		default:
			return nil
		}
	}
	return nil
}

func resize(input *os.File, guard *terminal.Guard, session *pty.Session) error {
	dimensions, err := terminal.FromTerminal(input)
	if err != nil {
		return err
	}
	if err := session.Resize(dimensions.Size()); err != nil {
		return fmt.Errorf("resize shell pseudoterminal: %w", err)
	}
	if err := guard.UpdateDimensions(dimensions); err != nil {
		return err
	}
	return nil
}

func suspend(input *os.File, guard *terminal.Guard, session *pty.Session) error {
	if err := guard.Restore(); err != nil {
		return fmt.Errorf("restore parent terminal before suspension: %w", err)
	}
	if err := forward(session, pty.SignalSuspend); err != nil {
		return err
	}
	if err := unix.Kill(unix.Getpid(), syscall.SIGSTOP); err != nil {
		return fmt.Errorf("stop transparent shell wrapper: %w", err)
	}
	if err := guard.EnterRaw(); err != nil {
		return err
	}
	if err := resize(input, guard, session); err != nil {
		return err
	}
	return forward(session, pty.SignalContinue)
}

func forward(session *pty.Session, signal pty.Signal) error {
	_, err := session.ForwardSignal(signal)
	if err != nil && !errors.Is(err, unix.ESRCH) {
		return fmt.Errorf("forward signal to shell foreground: %w", err)
	}
	return nil
}

func drainIntegration(descriptor *pty.Descriptor) error {
	buffer := make([]byte, ioBytes)
	for total := 0; total < queueBytes; {
		n, err := descriptor.Read(buffer)
		total += n
		if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
			return nil
		}
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return fmt.Errorf("drain shell integration stream: %w", err)
		}
		if n == 0 {
			return nil
		}
	}
	return nil
}

func readParentInput(file *os.File, queue *byteQueue) (bool, error) {
	buffer := queue.writable()
	if len(buffer) > ioBytes {
		buffer = buffer[:ioBytes]
	}
	n, err := unix.Read(int(file.Fd()), buffer)
	if n > 0 {
		queue.produced(n)
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("read parent terminal input: %w", err)
	}
	return n == 0, nil
}

func writeShellInput(descriptor *pty.Descriptor, queue *byteQueue) error {
	buffer := queue.readable()
	if len(buffer) > ioBytes {
		buffer = buffer[:ioBytes]
	}
	n, err := descriptor.Write(buffer)
	if n > 0 {
		queue.consumed(n)
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("write shell input: %w", err)
	}
	return nil
}

func readShellOutput(descriptor *pty.Descriptor, queue *byteQueue) (bool, error) {
	buffer := queue.writable()
	if len(buffer) > ioBytes {
		buffer = buffer[:ioBytes]
	}
	n, err := descriptor.Read(buffer)
	if n > 0 {
		queue.produced(n)
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return false, nil
	}
	if errors.Is(err, io.EOF) {
		return true, nil
	}
	if err != nil {
		return false, fmt.Errorf("read shell output: %w", err)
	}
	return false, nil
}

func writeParentOutput(file *os.File, queue *byteQueue) error {
	buffer := queue.readable()
	if len(buffer) > ioBytes {
		buffer = buffer[:ioBytes]
	}
	n, err := unix.Write(int(file.Fd()), buffer)
	if n > 0 {
		queue.consumed(n)
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("write parent terminal output: %w", err)
	}
	return nil
}
