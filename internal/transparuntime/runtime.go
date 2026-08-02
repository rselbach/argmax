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

	"github.com/rselbach/argmax/internal/inputrouter"
	"github.com/rselbach/argmax/internal/pty"
	"github.com/rselbach/argmax/internal/session"
	"github.com/rselbach/argmax/internal/shellcontrol"
	"github.com/rselbach/argmax/internal/shellevents"
	"github.com/rselbach/argmax/internal/shellintegration"
	"github.com/rselbach/argmax/internal/shellselect"
	"github.com/rselbach/argmax/internal/terminal"
	"golang.org/x/sys/unix"
)

const (
	queueBytes           = 256 * 1024
	maxPendingWriteBytes = 512 * 1024
	ioBytes              = 64 * 1024
	pollInterval         = 20 * time.Millisecond
	trailingDrainTime    = 500 * time.Millisecond
	signalQueueSize      = 32
	uiMaxSuggestions     = 100
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
	controller, err := newSessionController(config)
	if err != nil {
		return pty.ExitStatus{}, err
	}
	defer func() { err = errors.Join(err, controller.reducer.Close()) }()

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

	return eventLoop(config, guard, session, controller)
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

type writeDestination uint8

const (
	writePTY writeDestination = iota + 1
	writeControl
)

type pendingWriteGroup struct {
	probeResync uint64
}

type pendingWrite struct {
	destination writeDestination
	group       pendingWriteGroup
	hasGroup    bool
	bytes       []byte
	written     int
}

type pendingWrites struct {
	writes []pendingWrite
	bytes  int
}

func (pending *pendingWrites) available() int {
	return maxPendingWriteBytes - pending.bytes
}

func (pending *pendingWrites) empty() bool { return len(pending.writes) == 0 }

func (pending *pendingWrites) front() *pendingWrite {
	if len(pending.writes) == 0 {
		return nil
	}
	return &pending.writes[0]
}

func (pending *pendingWrites) push(destination writeDestination, bytes []byte) error {
	return pending.pushGrouped(destination, pendingWriteGroup{}, bytes)
}

func (pending *pendingWrites) pushGrouped(
	destination writeDestination,
	group pendingWriteGroup,
	bytes []byte,
) error {
	if len(bytes) == 0 {
		return nil
	}
	if len(bytes) > pending.available() {
		return errors.New("pending shell writes exceed 512 KiB")
	}
	pending.writes = append(pending.writes, pendingWrite{
		destination: destination,
		group:       group,
		hasGroup:    group.probeResync != 0,
		bytes:       append([]byte(nil), bytes...),
	})
	pending.bytes += len(bytes)
	return nil
}

func (pending *pendingWrites) advanceFront(written int) error {
	front := pending.front()
	if front == nil {
		return errors.New("pending-write progress without a write")
	}
	remaining := len(front.bytes) - front.written
	if written < 0 || written > remaining {
		return errors.New("transport reported invalid write progress")
	}
	front.written += written
	pending.bytes -= written
	if front.written == len(front.bytes) {
		clear(front.bytes)
		pending.writes = pending.writes[1:]
	}
	return nil
}

func (pending *pendingWrites) cancelProbeResync(request shellcontrol.ProbeResyncRequestID) {
	group := pendingWriteGroup{probeResync: request.Value()}
	preservePartialControl := false
	for index := range pending.writes {
		write := &pending.writes[index]
		if write.hasGroup && write.group == group &&
			write.destination == writeControl && write.written != 0 {
			preservePartialControl = true
			break
		}
	}

	kept := pending.writes[:0]
	for index := range pending.writes {
		write := pending.writes[index]
		if !write.hasGroup || write.group != group {
			kept = append(kept, write)
			continue
		}
		if preservePartialControl && write.destination == writeControl {
			write.hasGroup = false
			write.group = pendingWriteGroup{}
			kept = append(kept, write)
			continue
		}
		pending.bytes -= len(write.bytes) - write.written
		clear(write.bytes)
	}
	pending.writes = kept
}

func (pending *pendingWrites) discard(destination writeDestination) {
	groups := make(map[pendingWriteGroup]struct{})
	for _, write := range pending.writes {
		if write.destination == destination && write.hasGroup {
			groups[write.group] = struct{}{}
		}
	}
	kept := pending.writes[:0]
	for index := range pending.writes {
		write := pending.writes[index]
		_, grouped := groups[write.group]
		if write.destination != destination && (!write.hasGroup || !grouped) {
			kept = append(kept, write)
			continue
		}
		pending.bytes -= len(write.bytes) - write.written
		clear(write.bytes)
	}
	pending.writes = kept
}

type sessionController struct {
	decoder           *shellevents.Decoder
	reducer           *session.Reducer
	pending           pendingWrites
	syncProbeSequence []byte
	integrationEOF    bool
}

func newSessionController(config Config) (*sessionController, error) {
	return newSessionControllerForShell(config.Shell.Kind(), []byte(config.Cwd))
}

func newSessionControllerForShell(
	kind shellselect.Kind,
	cwd []byte,
) (*sessionController, error) {
	epoch := shellevents.InitialStreamEpoch()
	reducer, err := session.New(
		epoch,
		[]byte{0x12},
		[]byte("\x1b[Z"),
		nil,
		uiMaxSuggestions,
		cwd,
	)
	if err != nil {
		return nil, fmt.Errorf("construct session reducer: %w", err)
	}

	var probe string
	switch kind {
	case shellselect.Bash, shellselect.Zsh:
		probe = shellintegration.SyncProbeSequence
	case shellselect.Fish:
		probe = shellintegration.FishSyncProbeSequence
	default:
		closeErr := reducer.Close()
		return nil, errors.Join(shellintegration.ErrUnsupportedShell, closeErr)
	}
	return &sessionController{
		decoder: shellevents.NewDecoder(epoch), reducer: reducer,
		syncProbeSequence: []byte(probe),
	}, nil
}

func (controller *sessionController) routeInput(input []byte) error {
	for len(input) != 0 {
		reduction := controller.reducer.RouteInput(input)
		consumed := reduction.ConsumedBytes()
		if consumed <= 0 || consumed > len(input) {
			return errors.New("session reducer reported invalid input progress")
		}
		if err := controller.applyEffects(reduction.Effects()); err != nil {
			return err
		}
		input = input[consumed:]
	}
	return nil
}

func (controller *sessionController) finishInput() error {
	reduction := controller.reducer.FinishInput()
	if reduction.ConsumedBytes() != 0 {
		return errors.New("session reducer consumed bytes while finishing input")
	}
	return controller.applyEffects(reduction.Effects())
}

func (controller *sessionController) applyFrame(frame shellevents.DecodedFrame) error {
	_, effects := controller.reducer.ApplyShellFrame(frame)
	return controller.applyEffects(effects)
}

func (controller *sessionController) finishIntegration() error {
	if controller.integrationEOF {
		return nil
	}
	controller.integrationEOF = true
	if frame, ok := controller.decoder.Finish(); ok {
		if err := controller.applyFrame(frame); err != nil {
			return err
		}
	}
	controller.pending.discard(writeControl)
	return nil
}

func (controller *sessionController) observeShellOutput() error {
	return controller.applyEffects(controller.reducer.ObserveShellOutput())
}

func (controller *sessionController) applyEffects(batch session.EffectBatch) error {
	var replacement *session.BufferReplacement
	staged := make([][]byte, 0, 1)
	for _, effect := range batch.Effects() {
		switch effect.Kind() {
		case session.EffectForwardInput:
			input, ok := effect.ForwardInput()
			if !ok {
				return errors.New("forward-input effect lacked bytes")
			}
			if replacement != nil {
				staged = append(staged, input)
				continue
			}
			if err := controller.pending.push(writePTY, input); err != nil {
				return err
			}
		case session.EffectReplaceBuffer:
			value, ok := effect.Replacement()
			if !ok {
				return errors.New("replace-buffer effect lacked a replacement")
			}
			if replacement != nil {
				return errors.New("multiple replacements preceded one synchronization request")
			}
			replacement = &value
		case session.EffectRequestBufferSync:
			nonce, ok := effect.BufferSyncNonce()
			if !ok {
				return errors.New("buffer-sync effect lacked a nonce")
			}
			if replacement != nil {
				if err := controller.enqueueReplacement(*replacement, nonce); err != nil {
					return err
				}
				replacement = nil
				for _, input := range staged {
					if err := controller.pending.push(writePTY, input); err != nil {
						return err
					}
				}
				staged = staged[:0]
			}
			if err := controller.pending.push(writePTY, controller.syncProbeSequence); err != nil {
				return err
			}
		case session.EffectRequestProbeResync:
			request, ok := effect.ProbeResyncRequest()
			if !ok {
				return errors.New("probe-resync effect lacked a request")
			}
			if replacement != nil || len(staged) != 0 {
				return errors.New("probe resync followed an incomplete buffer replacement")
			}
			if err := controller.enqueueProbeResync(request); err != nil {
				return err
			}
		case session.EffectCancelProbeResync:
			request, ok := effect.CancelledProbeResyncRequest()
			if !ok {
				return errors.New("probe-resync cancellation lacked a request")
			}
			controller.pending.cancelProbeResync(request)
		case session.EffectClearOverlay, session.EffectRefreshOverlay,
			session.EffectStartQuery, session.EffectModeChanged:
			// Presentation and provider work are intentionally absent in this runtime.
		case session.EffectFault:
			fault, ok := effect.Fault()
			if !ok {
				return errors.New("session fault effect lacked a fault")
			}
			return fmt.Errorf("session reducer fault: %w", fault)
		default:
			return fmt.Errorf("unknown session effect kind %d", effect.Kind())
		}
	}
	if replacement != nil || len(staged) != 0 {
		return errors.New("buffer replacement lacked a following synchronization request")
	}
	return nil
}

func (controller *sessionController) enqueueReplacement(
	replacement session.BufferReplacement,
	nonce shellevents.SnapshotNonce,
) error {
	request, err := shellcontrol.NewControlRequestID(nonce.Value())
	if err != nil {
		return fmt.Errorf("construct replacement request: %w", err)
	}
	control, err := shellcontrol.NewReplacementControl(
		request,
		replacement.Text(),
		replacement.Cursor(),
	)
	if err != nil {
		return fmt.Errorf("encode shell replacement: %w", err)
	}
	return controller.pending.push(writeControl, control.Encode().Bytes())
}

func (controller *sessionController) enqueueProbeResync(
	request shellcontrol.ProbeResyncRequestID,
) error {
	control := shellcontrol.NewProbeResyncControl(request).Encode().Bytes()
	needed := len(control) + len(controller.syncProbeSequence)
	if needed > controller.pending.available() {
		return errors.New("pending shell writes exceed 512 KiB")
	}
	group := pendingWriteGroup{probeResync: request.Value()}
	if err := controller.pending.pushGrouped(writeControl, group, control); err != nil {
		return err
	}
	if err := controller.pending.pushGrouped(
		writePTY,
		group,
		controller.syncProbeSequence,
	); err != nil {
		controller.pending.cancelProbeResync(request)
		return err
	}
	return nil
}

func eventLoop(
	config Config,
	guard *terminal.Guard,
	ptySession *pty.Session,
	controller *sessionController,
) (pty.ExitStatus, error) {
	signals := make(chan os.Signal, signalQueueSize)
	osSignal.Notify(
		signals,
		syscall.SIGWINCH, syscall.SIGINT, syscall.SIGQUIT, syscall.SIGTERM,
		syscall.SIGHUP, syscall.SIGTSTP, syscall.SIGCONT, syscall.SIGPIPE,
	)
	defer osSignal.Stop(signals)

	outputQueue := newByteQueue()
	inputEOF := false
	inputClosed := false
	masterEOF := false
	var childExit *pty.ExitStatus
	var drainDeadline time.Time

	for {
		if err := drainSignals(signals, config.Input, guard, ptySession); err != nil {
			return statusOrZero(childExit), err
		}
		if childExit == nil {
			status, err := ptySession.TryWait()
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
			if masterEOF && outputQueue.length() == 0 && controller.pending.empty() {
				if err := controller.finishIntegration(); err != nil {
					return *childExit, err
				}
				return *childExit, nil
			}
			if !time.Now().Before(drainDeadline) {
				if err := controller.finishIntegration(); err != nil {
					return *childExit, err
				}
				return *childExit, nil
			}
		}

		pollDescriptors := []unix.PollFd{
			{Fd: int32(config.Input.Fd())},
			{Fd: int32(config.Output.Fd())},
			{Fd: int32(ptySession.Master().FD())},
			{Fd: int32(ptySession.Integration().FD())},
		}
		if !inputEOF && childExit == nil &&
			controller.pending.available() > inputReadReserve(controller) {
			pollDescriptors[0].Events = unix.POLLIN
		}
		if outputQueue.length() != 0 {
			pollDescriptors[1].Events = unix.POLLOUT
		}
		if !masterEOF && len(outputQueue.writable()) != 0 {
			pollDescriptors[2].Events |= unix.POLLIN
		}
		if !controller.integrationEOF {
			pollDescriptors[3].Events |= unix.POLLIN
		}
		if front := controller.pending.front(); front != nil {
			switch front.destination {
			case writePTY:
				pollDescriptors[2].Events |= unix.POLLOUT
			case writeControl:
				pollDescriptors[3].Events |= unix.POLLOUT
			}
		} else if inputEOF && !inputClosed {
			pollDescriptors[2].Events |= unix.POLLOUT
		}

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

		if !controller.integrationEOF &&
			pollDescriptors[3].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) != 0 {
			if err := drainIntegration(ptySession.Integration(), controller); err != nil {
				return statusOrZero(childExit), err
			}
		}
		if outputQueue.length() != 0 &&
			pollDescriptors[1].Revents&(unix.POLLOUT|unix.POLLHUP|unix.POLLERR) != 0 {
			if err := writeParentOutput(config.Output, &outputQueue); err != nil {
				return statusOrZero(childExit), err
			}
		}
		if !masterEOF &&
			pollDescriptors[2].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) != 0 {
			eof, err := readShellOutput(ptySession.Master(), &outputQueue, controller)
			if err != nil {
				return statusOrZero(childExit), err
			}
			masterEOF = eof
		}
		if !controller.pending.empty() {
			if err := flushPending(ptySession, controller, childExit != nil); err != nil {
				return statusOrZero(childExit), err
			}
		}
		if controller.pending.empty() && inputEOF && !inputClosed &&
			pollDescriptors[2].Revents&(unix.POLLOUT|unix.POLLHUP|unix.POLLERR) != 0 {
			progress, err := ptySession.CloseInput()
			if err != nil {
				return statusOrZero(childExit), fmt.Errorf("close shell input: %w", err)
			}
			inputClosed = progress.Closed
		}
		if !inputEOF && childExit == nil &&
			pollDescriptors[0].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) != 0 {
			eof, err := readParentInput(config.Input, controller)
			if err != nil {
				return statusOrZero(childExit), err
			}
			if eof {
				if err := controller.finishInput(); err != nil {
					return statusOrZero(childExit), err
				}
				inputEOF = true
			}
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

func drainIntegration(
	descriptor *pty.Descriptor,
	controller *sessionController,
) error {
	buffer := make([]byte, ioBytes)
	for total := 0; total < queueBytes; {
		n, err := descriptor.Read(buffer)
		total += n
		var applyErr error
		if n > 0 {
			controller.decoder.Push(buffer[:n], func(frame shellevents.DecodedFrame) {
				if applyErr == nil {
					applyErr = controller.applyFrame(frame)
				}
			})
			if applyErr != nil {
				return applyErr
			}
		}
		if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
			return nil
		}
		if errors.Is(err, io.EOF) {
			return controller.finishIntegration()
		}
		if err != nil {
			return fmt.Errorf("read shell integration stream: %w", err)
		}
		if n == 0 {
			return controller.finishIntegration()
		}
	}
	return nil
}

func inputReadReserve(controller *sessionController) int {
	return shellcontrol.MaxControlWireBytes + inputrouter.MaxRetainedPrefixBytes +
		len(controller.syncProbeSequence)
}

func readParentInput(file *os.File, controller *sessionController) (bool, error) {
	available := controller.pending.available() - inputReadReserve(controller)
	if available <= 0 {
		return false, nil
	}
	buffer := make([]byte, min(ioBytes, available))
	n, err := unix.Read(int(file.Fd()), buffer)
	if n > 0 {
		if routeErr := controller.routeInput(buffer[:n]); routeErr != nil {
			return false, routeErr
		}
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("read parent terminal input: %w", err)
	}
	return n == 0, nil
}

func flushPending(
	ptySession *pty.Session,
	controller *sessionController,
	childExited bool,
) error {
	for total := 0; total < queueBytes; {
		front := controller.pending.front()
		if front == nil {
			return nil
		}
		buffer := front.bytes[front.written:]
		if len(buffer) > ioBytes {
			buffer = buffer[:ioBytes]
		}

		var n int
		var err error
		switch front.destination {
		case writePTY:
			n, err = ptySession.Master().Write(buffer)
		case writeControl:
			if controller.integrationEOF {
				controller.pending.discard(writeControl)
				continue
			}
			n, err = ptySession.Integration().Write(buffer)
		default:
			return errors.New("pending write has an unknown destination")
		}
		if n > 0 {
			if progressErr := controller.pending.advanceFront(n); progressErr != nil {
				return progressErr
			}
			total += n
		}
		if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
			return nil
		}
		if err == nil {
			if n == 0 {
				return nil
			}
			continue
		}
		if front.destination == writeControl &&
			(errors.Is(err, pty.ErrClosed) || errors.Is(err, unix.EPIPE) ||
				errors.Is(err, unix.ECONNRESET)) {
			if finishErr := controller.finishIntegration(); finishErr != nil {
				return finishErr
			}
			continue
		}
		if front.destination == writePTY && childExited {
			controller.pending.discard(writePTY)
			continue
		}
		return fmt.Errorf("write shell transport: %w", err)
	}
	return nil
}

func readShellOutput(
	descriptor *pty.Descriptor,
	queue *byteQueue,
	controller *sessionController,
) (bool, error) {
	buffer := queue.writable()
	if len(buffer) > ioBytes {
		buffer = buffer[:ioBytes]
	}
	n, err := descriptor.Read(buffer)
	if n > 0 {
		if observeErr := controller.observeShellOutput(); observeErr != nil {
			return false, observeErr
		}
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
