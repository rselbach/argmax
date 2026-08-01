//go:build linux || darwin

// Package pty starts a process on a native pseudoterminal and exposes bounded,
// nonblocking byte transports and process lifecycle operations.
package pty

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sys/unix"
)

const (
	// MaxIOBytes is the largest buffer accepted by one transport operation.
	MaxIOBytes = 64 * 1024
	// MaxArgs is the largest argument count accepted by Start.
	MaxArgs = 256
	// MaxArgumentBytes is the largest aggregate argument payload accepted by Start.
	MaxArgumentBytes = 64 * 1024

	maxMarkerBytes = 128

	shutdownGrace = 100 * time.Millisecond
	waitPoll      = 5 * time.Millisecond

	markerEnvironment       = "ARGMAX_PRIVATE_SESSION"
	eventFDEnvironment      = "ARGMAX_EVENT_FD"
	controlEnvironment      = "ARGMAX_CONTROL_FD"
	activeShellEnvironment  = "ARGMAX_ACTIVE_SHELL"
	sessionOwnerEnvironment = "ARGMAX_SESSION_OWNER_PID"
)

var (
	// ErrIOTooLarge reports a transport operation larger than MaxIOBytes.
	ErrIOTooLarge = errors.New("pty transport buffer exceeds limit")
	// ErrClosed reports an operation on a closed transport descriptor.
	ErrClosed = errors.New("pty transport descriptor is closed")
	// ErrInputClosed reports a write after terminal EOF has started.
	ErrInputClosed = errors.New("pty input is closed")
	// ErrUnsupportedSignal reports a signal outside the forwarding allowlist.
	ErrUnsupportedSignal = errors.New("pty signal is not supported")
	// ErrShutdownTimeout reports a child that could not be reaped within the
	// bounded shutdown grace periods.
	ErrShutdownTimeout = errors.New("pty child did not terminate during shutdown")
)

// Size is an exact terminal window size in cells and pixels.
type Size struct {
	Rows        uint16
	Cols        uint16
	PixelWidth  uint16
	PixelHeight uint16
}

func (size Size) winsize() *unix.Winsize {
	return &unix.Winsize{
		Row:    size.Rows,
		Col:    size.Cols,
		Xpixel: size.PixelWidth,
		Ypixel: size.PixelHeight,
	}
}

// Config describes one direct child process start.
type Config struct {
	Executable string
	Args       []string
	Cwd        string
	Size       Size
	Marker     string
	// ShellKind, when nonempty, must be bash, zsh, or fish. Start then sets
	// SHELL to Executable and ARGMAX_ACTIVE_SHELL to ShellKind for the child.
	ShellKind string
}

// String returns a representation that redacts paths, arguments, and marker.
func (config Config) String() string {
	return fmt.Sprintf("pty.Config{args:%d, size:%+v, private:<redacted>}", len(config.Args), config.Size)
}

// GoString returns a representation that redacts paths, arguments, and marker.
func (config Config) GoString() string { return config.String() }

// Descriptor owns one nonblocking Unix descriptor.
//
// Read and Write issue exactly one unix.Read or unix.Write call. Retryable
// EAGAIN and EINTR values are returned unchanged. A zero-length read from a
// closed peer is io.EOF, as is Linux's PTY-master EIO-after-slave-close. Close
// is idempotent.
type Descriptor struct {
	mu          sync.Mutex
	fd          int
	master      bool
	inputClosed bool
}

func newDescriptor(fd int, master bool) *Descriptor {
	return &Descriptor{fd: fd, master: master}
}

// FD returns the owned descriptor number, or -1 after Close.
// The caller must not close it directly.
func (descriptor *Descriptor) FD() int {
	descriptor.mu.Lock()
	defer descriptor.mu.Unlock()
	return descriptor.fd
}

// Read performs one bounded raw read.
func (descriptor *Descriptor) Read(buffer []byte) (int, error) {
	if len(buffer) > MaxIOBytes {
		return 0, ErrIOTooLarge
	}

	descriptor.mu.Lock()
	defer descriptor.mu.Unlock()
	if descriptor.fd < 0 {
		return 0, io.EOF
	}
	n, err := unix.Read(descriptor.fd, buffer)
	if n < 0 {
		n = 0
	}
	if descriptor.master {
		err = normalizeMasterReadError(err)
	}
	if n == 0 && err == nil && len(buffer) > 0 {
		return 0, io.EOF
	}
	return n, err
}

// Write performs one bounded raw write.
func (descriptor *Descriptor) Write(buffer []byte) (int, error) {
	if len(buffer) > MaxIOBytes {
		return 0, ErrIOTooLarge
	}

	descriptor.mu.Lock()
	defer descriptor.mu.Unlock()
	if descriptor.fd < 0 {
		return 0, ErrClosed
	}
	if descriptor.master && descriptor.inputClosed {
		return 0, ErrInputClosed
	}
	n, err := unix.Write(descriptor.fd, buffer)
	if n < 0 {
		n = 0
	}
	return n, err
}

// Close closes the descriptor. Repeated calls succeed.
func (descriptor *Descriptor) Close() error {
	descriptor.mu.Lock()
	defer descriptor.mu.Unlock()
	if descriptor.fd < 0 {
		return nil
	}
	fd := descriptor.fd
	descriptor.fd = -1
	return unix.Close(fd)
}

// String returns a descriptor representation without its numeric value.
func (descriptor *Descriptor) String() string { return "pty.Descriptor(<redacted>)" }

// GoString returns a descriptor representation without its numeric value.
func (descriptor *Descriptor) GoString() string { return descriptor.String() }

// ExitKind identifies how the direct child ceased to be waitable.
type ExitKind uint8

const (
	// ExitExited identifies normal process exit.
	ExitExited ExitKind = iota + 1
	// ExitSignaled identifies termination by a native signal.
	ExitSignaled
	// ExitExternallyReaped identifies a child reaped by another owner.
	ExitExternallyReaped
)

// ExitStatus preserves the direct child's exact native exit outcome.
type ExitStatus struct {
	Kind   ExitKind
	Code   int
	Signal syscall.Signal
}

// Success reports a normal zero exit.
func (status ExitStatus) Success() bool {
	return status.Kind == ExitExited && status.Code == 0
}

// WrapperCode returns the conventional shell wrapper code when representable.
func (status ExitStatus) WrapperCode() (int, bool) {
	switch status.Kind {
	case ExitExited:
		return status.Code, true
	case ExitSignaled:
		return 128 + int(status.Signal), true
	default:
		return 0, false
	}
}

// ForegroundState describes current ownership of the PTY foreground.
type ForegroundState struct {
	Available         bool
	ProcessGroup      int
	ShellOwnsTerminal bool
}

// Signal is one signal allowed through foreground forwarding.
type Signal uint8

const (
	SignalInterrupt Signal = iota + 1
	SignalQuit
	SignalTerminate
	SignalHangup
	SignalSuspend
	SignalContinue
)

// SignalDelivery describes one foreground forwarding attempt.
type SignalDelivery uint8

const (
	SignalDelivered SignalDelivery = iota + 1
	SignalNoForegroundProcessGroup
	SignalRefusedWrapperProcessGroup
)

// CloseInputResult reports cumulative progress queueing terminal EOF.
type CloseInputResult struct {
	Closed  bool
	Written int
	Total   int
}

type eofProgress struct {
	bytes   [2]byte
	length  int
	written int
}

// Session owns the parent PTY and integration descriptors and the sole right
// to reap the direct child.
//
// Wait4 is used directly rather than os.Process.Wait. No exec-managed pipes or
// goroutines exist, so serializing wait4 here preserves exact status while
// keeping one unambiguous reaping owner.
type Session struct {
	master      *Descriptor
	integration *Descriptor
	pid         int

	waitMu sync.Mutex
	exit   *ExitStatus

	eofMu       sync.Mutex
	eofProgress *eofProgress

	closeOnce sync.Once
	closeErr  error
}

// Start validates all caller input, allocates the transports, and starts the
// configured executable directly.
func Start(config Config) (*Session, error) {
	if err := validateConfig(config); err != nil {
		return nil, err
	}

	masterFD, slaveFD, err := openPTY()
	if err != nil {
		return nil, fmt.Errorf("allocate pseudoterminal: %w", err)
	}
	master := newDescriptor(masterFD, true)
	closeAllocatedPTY := func() error {
		return errors.Join(master.Close(), unix.Close(slaveFD))
	}

	if err := setSize(masterFD, config.Size); err != nil {
		return nil, errors.Join(
			fmt.Errorf("set pseudoterminal size: %w", err),
			closeAllocatedPTY(),
		)
	}
	if err := unix.SetNonblock(masterFD, true); err != nil {
		return nil, errors.Join(
			fmt.Errorf("make pseudoterminal nonblocking: %w", err),
			closeAllocatedPTY(),
		)
	}

	pair, err := openSocketPair()
	if err != nil {
		return nil, errors.Join(
			fmt.Errorf("allocate integration socketpair: %w", err),
			closeAllocatedPTY(),
		)
	}
	integration := newDescriptor(pair[0], false)

	slave := os.NewFile(uintptr(slaveFD), "pty-slave")
	childIntegration := os.NewFile(uintptr(pair[1]), "integration-child")
	if slave == nil || childIntegration == nil {
		cleanupErr := errors.Join(
			master.Close(), integration.Close(),
			closeFileDescriptor(slave, slaveFD),
			closeFileDescriptor(childIntegration, pair[1]),
		)
		return nil, errors.Join(errors.New("construct child descriptors"), cleanupErr)
	}

	argv := make([]string, 1, len(config.Args)+1)
	argv[0] = config.Executable
	argv = append(argv, config.Args...)
	process, startErr := os.StartProcess(config.Executable, argv, &os.ProcAttr{
		Dir:   config.Cwd,
		Env:   childEnvironment(config),
		Files: []*os.File{slave, slave, slave, childIntegration},
		Sys: &syscall.SysProcAttr{
			Setsid:  true,
			Setctty: true,
			Ctty:    0,
		},
	})
	childCloseErr := errors.Join(slave.Close(), childIntegration.Close())
	if startErr != nil {
		cleanupErr := errors.Join(master.Close(), integration.Close(), childCloseErr)
		return nil, errors.Join(redactedStartError(startErr), cleanupErr)
	}

	session := &Session{
		master:      master,
		integration: integration,
		pid:         process.Pid,
	}
	releaseErr := process.Release()
	if childCloseErr != nil || releaseErr != nil {
		shutdownErr := session.Shutdown()
		return nil, errors.Join(
			errors.New("finalize parent child descriptors"),
			childCloseErr, releaseErr, shutdownErr,
		)
	}
	return session, nil
}

// PID returns the direct child's process identifier.
func (session *Session) PID() int { return session.pid }

// Master returns the parent-owned PTY master transport.
func (session *Session) Master() *Descriptor { return session.master }

// Integration returns the parent-owned full-duplex integration transport.
func (session *Session) Integration() *Descriptor { return session.integration }

// Resize applies an exact nonzero cell and pixel size to the PTY.
func (session *Session) Resize(size Size) error {
	if err := validateSize(size); err != nil {
		return err
	}
	session.master.mu.Lock()
	defer session.master.mu.Unlock()
	if session.master.fd < 0 {
		return ErrClosed
	}
	return setSize(session.master.fd, size)
}

// Size returns the exact current PTY cell and pixel size.
func (session *Session) Size() (Size, error) {
	session.master.mu.Lock()
	defer session.master.mu.Unlock()
	if session.master.fd < 0 {
		return Size{}, ErrClosed
	}
	value, err := unix.IoctlGetWinsize(session.master.fd, unix.TIOCGWINSZ)
	if err != nil {
		return Size{}, err
	}
	return Size{
		Rows:        value.Row,
		Cols:        value.Col,
		PixelWidth:  value.Xpixel,
		PixelHeight: value.Ypixel,
	}, nil
}

// ForegroundProcessGroup returns the PTY's current positive foreground group.
func (session *Session) ForegroundProcessGroup() (int, error) {
	session.master.mu.Lock()
	defer session.master.mu.Unlock()
	if session.master.fd < 0 {
		return 0, ErrClosed
	}
	group, err := unix.IoctlGetInt(session.master.fd, unix.TIOCGPGRP)
	if err != nil {
		return 0, err
	}
	if group <= 0 {
		return 0, errors.New("pty foreground process group is unavailable")
	}
	return group, nil
}

// ForegroundState reports whether the shell or one of its jobs owns the PTY.
func (session *Session) ForegroundState() ForegroundState {
	group, err := session.ForegroundProcessGroup()
	if err != nil {
		return ForegroundState{}
	}
	return ForegroundState{
		Available:         true,
		ProcessGroup:      group,
		ShellOwnsTerminal: group == session.pid,
	}
}

// ForwardSignal sends an allowed signal to the current positive foreground
// process group. The wrapper's own process group is always refused.
func (session *Session) ForwardSignal(signal Signal) (SignalDelivery, error) {
	native, err := signal.native()
	if err != nil {
		return 0, err
	}
	if session.hasExit() {
		return SignalNoForegroundProcessGroup, nil
	}
	group, err := session.ForegroundProcessGroup()
	if err != nil {
		if errors.Is(err, ErrClosed) {
			return SignalNoForegroundProcessGroup, nil
		}
		return SignalNoForegroundProcessGroup, nil
	}
	return forwardProcessGroup(group, native)
}

func forwardProcessGroup(group int, signal syscall.Signal) (SignalDelivery, error) {
	if group <= 0 {
		return SignalNoForegroundProcessGroup, nil
	}
	if group == unix.Getpgrp() {
		return SignalRefusedWrapperProcessGroup, nil
	}
	if err := unix.Kill(-group, signal); err != nil {
		return 0, err
	}
	return SignalDelivered, nil
}

func (signal Signal) native() (syscall.Signal, error) {
	switch signal {
	case SignalInterrupt:
		return syscall.SIGINT, nil
	case SignalQuit:
		return syscall.SIGQUIT, nil
	case SignalTerminate:
		return syscall.SIGTERM, nil
	case SignalHangup:
		return syscall.SIGHUP, nil
	case SignalSuspend:
		return syscall.SIGTSTP, nil
	case SignalContinue:
		return syscall.SIGCONT, nil
	default:
		return 0, ErrUnsupportedSignal
	}
}

// CloseInput queues the terminal's current VEOF without blocking. A newline is
// prefixed only while the direct shell owns the foreground. Pending progress is
// retained so a later call writes only the remaining suffix.
func (session *Session) CloseInput() (CloseInputResult, error) {
	session.eofMu.Lock()
	defer session.eofMu.Unlock()

	session.master.mu.Lock()
	defer session.master.mu.Unlock()
	if session.master.fd < 0 {
		return CloseInputResult{Closed: true}, nil
	}
	if session.eofProgress == nil {
		eof, err := terminalEOF(session.master.fd)
		if err != nil {
			return CloseInputResult{}, err
		}
		group, foregroundErr := unix.IoctlGetInt(session.master.fd, unix.TIOCGPGRP)
		shellOwnsForeground := foregroundErr == nil && group > 0 && group == session.pid
		progress := &eofProgress{}
		switch {
		case eof == 0:
		case shellOwnsForeground:
			progress.bytes = [2]byte{'\n', eof}
			progress.length = 2
		default:
			progress.bytes[0] = eof
			progress.length = 1
		}
		session.eofProgress = progress
		session.master.inputClosed = true
	}

	progress := session.eofProgress
	if progress.written == progress.length {
		return CloseInputResult{
			Closed: true, Written: progress.written, Total: progress.length,
		}, nil
	}
	n, err := unix.Write(
		session.master.fd,
		progress.bytes[progress.written:progress.length],
	)
	if n > 0 {
		progress.written += n
	}
	result := CloseInputResult{Written: progress.written, Total: progress.length}
	if err == nil && progress.written == progress.length {
		result.Closed = true
		return result, nil
	}
	if errors.Is(err, unix.EAGAIN) || errors.Is(err, unix.EINTR) {
		return result, nil
	}
	return result, err
}

// TryWait polls child status without blocking. A nil status means the child is
// still running. The first terminal result is cached permanently.
func (session *Session) TryWait() (*ExitStatus, error) {
	return session.tryWait()
}

// Wait waits for the child using nonblocking wait4 polls. Repeated calls return
// the first exact result and never attempt to reap twice.
func (session *Session) Wait() (ExitStatus, error) {
	for {
		status, err := session.tryWait()
		if err != nil {
			return ExitStatus{}, err
		}
		if status != nil {
			return *status, nil
		}
		time.Sleep(waitPoll)
	}
}

func (session *Session) tryWait() (*ExitStatus, error) {
	session.waitMu.Lock()
	defer session.waitMu.Unlock()
	if session.exit != nil {
		copy := *session.exit
		return &copy, nil
	}

	var native unix.WaitStatus
	waited, err := unix.Wait4(session.pid, &native, unix.WNOHANG, nil)
	if errors.Is(err, unix.EINTR) || waited == 0 {
		return nil, nil
	}
	if errors.Is(err, unix.ECHILD) {
		status := ExitStatus{Kind: ExitExternallyReaped}
		session.exit = &status
		return &status, nil
	}
	if err != nil {
		return nil, err
	}
	status, err := exitStatus(native)
	if err != nil {
		return nil, err
	}
	session.exit = &status
	return &status, nil
}

func exitStatus(native unix.WaitStatus) (ExitStatus, error) {
	switch {
	case native.Exited():
		return ExitStatus{Kind: ExitExited, Code: native.ExitStatus()}, nil
	case native.Signaled():
		return ExitStatus{Kind: ExitSignaled, Signal: native.Signal()}, nil
	default:
		return ExitStatus{}, errors.New("pty wait returned a nonterminal status")
	}
}

func (session *Session) hasExit() bool {
	session.waitMu.Lock()
	defer session.waitMu.Unlock()
	return session.exit != nil
}

// Shutdown contains the attached session, reaps without an unbounded wait, and
// closes every parent descriptor. It is idempotent. Close is an alias.
func (session *Session) Shutdown() error {
	session.closeOnce.Do(func() {
		// Descriptor closure is guaranteed even if future maintenance introduces
		// a panic in the signal/reap path.
		defer func() {
			session.closeErr = errors.Join(
				session.closeErr,
				session.master.Close(),
				session.integration.Close(),
			)
		}()
		session.closeErr = session.shutdownProcess()
	})
	return session.closeErr
}

// Close performs bounded session shutdown. Repeated calls return the first result.
func (session *Session) Close() error { return session.Shutdown() }

func (session *Session) shutdownProcess() error {
	status, err := session.TryWait()
	if err != nil {
		return err
	}
	if status != nil {
		// Once the child has been reaped, its numeric process-group authority is
		// gone. Deliberately detached sessions remain outside containment.
		return nil
	}

	captured := session.shutdownGroups()
	var signalErr error
	signalErr = errors.Join(signalErr, session.signalTargets(captured, syscall.SIGHUP))
	if session.pollShutdown(time.Now().Add(shutdownGrace), captured) {
		return signalErr
	}

	escalation := session.revalidateForeground(captured)
	signalErr = errors.Join(signalErr, session.signalTargets(escalation, syscall.SIGKILL))
	if session.pollShutdown(time.Now().Add(shutdownGrace), escalation) {
		return signalErr
	}
	_, waitErr := session.TryWait()
	return errors.Join(signalErr, waitErr, ErrShutdownTimeout)
}

type processGroups struct {
	foreground int
	shell      int
}

func (session *Session) shutdownGroups() processGroups {
	wrapper := unix.Getpgrp()
	foreground, _ := session.ForegroundProcessGroup()
	if foreground <= 0 || foreground == wrapper {
		foreground = 0
	}
	shell := session.pid
	if shell <= 0 || shell == wrapper || shell == foreground {
		shell = 0
	}
	return processGroups{foreground: foreground, shell: shell}
}

func (session *Session) revalidateForeground(captured processGroups) processGroups {
	if captured.foreground != 0 {
		current, err := session.ForegroundProcessGroup()
		if err != nil || current != captured.foreground {
			captured.foreground = 0
		}
	}
	return captured
}

func (session *Session) signalTargets(groups processGroups, signal syscall.Signal) error {
	var result error
	for _, group := range []int{groups.foreground, groups.shell} {
		if group == 0 {
			continue
		}
		if err := unix.Kill(-group, signal); err != nil && !errors.Is(err, unix.ESRCH) {
			result = errors.Join(result, err)
		}
	}
	if !session.hasExit() {
		if err := unix.Kill(session.pid, signal); err != nil && !errors.Is(err, unix.ESRCH) {
			result = errors.Join(result, err)
		}
	}
	return result
}

func (session *Session) pollShutdown(deadline time.Time, groups processGroups) bool {
	for {
		status, _ := session.TryWait()
		if status != nil && !groupsAlive(groups) {
			return true
		}
		if !time.Now().Before(deadline) {
			return false
		}
		time.Sleep(waitPoll)
	}
}

func groupsAlive(groups processGroups) bool {
	for _, group := range []int{groups.foreground, groups.shell} {
		if group == 0 {
			continue
		}
		if err := unix.Kill(-group, 0); !errors.Is(err, unix.ESRCH) {
			return true
		}
	}
	return false
}

// String returns a representation without process, descriptor, or configuration values.
func (session *Session) String() string { return "pty.Session(<redacted>)" }

// GoString returns a representation without process, descriptor, or configuration values.
func (session *Session) GoString() string { return session.String() }

func validateConfig(config Config) error {
	if err := validateSize(config.Size); err != nil {
		return err
	}
	if !filepath.IsAbs(config.Executable) || strings.IndexByte(config.Executable, 0) >= 0 {
		return errors.New("pty executable must be an absolute path")
	}
	executable, err := os.Stat(config.Executable)
	if err != nil || !executable.Mode().IsRegular() || unix.Access(config.Executable, unix.X_OK) != nil {
		return errors.New("pty executable must be a regular executable file")
	}
	if !filepath.IsAbs(config.Cwd) || strings.IndexByte(config.Cwd, 0) >= 0 {
		return errors.New("pty working directory must be absolute")
	}
	directory, err := os.Stat(config.Cwd)
	if err != nil || !directory.IsDir() {
		return errors.New("pty working directory must be an existing directory")
	}
	if len(config.Args) > MaxArgs {
		return errors.New("pty argument count exceeds limit")
	}
	argumentBytes := 0
	for _, argument := range config.Args {
		if strings.IndexByte(argument, 0) >= 0 {
			return errors.New("pty argument contains NUL")
		}
		if len(argument) > MaxArgumentBytes-argumentBytes {
			return errors.New("pty arguments exceed byte limit")
		}
		argumentBytes += len(argument)
	}
	if config.Marker == "" {
		return errors.New("pty private marker must not be empty")
	}
	if config.ShellKind != "" && config.ShellKind != "bash" &&
		config.ShellKind != "zsh" && config.ShellKind != "fish" {
		return errors.New("pty shell kind must be bash, zsh, or fish")
	}
	if len(config.Marker) > maxMarkerBytes {
		return errors.New("pty private marker is too long")
	}
	for _, char := range []byte(config.Marker) {
		if (char < 'a' || char > 'z') &&
			(char < 'A' || char > 'Z') &&
			(char < '0' || char > '9') &&
			char != '-' && char != '_' && char != '.' {
			return errors.New("pty private marker contains an unsupported character")
		}
	}
	return nil
}

func validateSize(size Size) error {
	if size.Rows == 0 || size.Cols == 0 {
		return errors.New("pty rows and columns must be nonzero")
	}
	return nil
}

func childEnvironment(config Config) []string {
	replaced := map[string]struct{}{
		markerEnvironment:       {},
		eventFDEnvironment:      {},
		controlEnvironment:      {},
		sessionOwnerEnvironment: {},
	}
	if config.ShellKind != "" {
		replaced[activeShellEnvironment] = struct{}{}
		replaced["SHELL"] = struct{}{}
	}
	environment := os.Environ()
	child := make([]string, 0, len(environment)+5)
	for _, entry := range environment {
		name, _, _ := strings.Cut(entry, "=")
		if _, ok := replaced[name]; !ok {
			child = append(child, entry)
		}
	}
	child = append(
		child,
		markerEnvironment+"="+config.Marker,
		eventFDEnvironment+"=3",
		controlEnvironment+"=3",
	)
	if config.ShellKind != "" {
		child = append(
			child,
			activeShellEnvironment+"="+config.ShellKind,
			"SHELL="+config.Executable,
		)
	}
	return child
}

func setSize(fd int, size Size) error {
	return unix.IoctlSetWinsize(fd, unix.TIOCSWINSZ, size.winsize())
}

func closeFileDescriptor(file *os.File, fd int) error {
	if file == nil {
		return unix.Close(fd)
	}
	return file.Close()
}

func redactedStartError(err error) error {
	var errno syscall.Errno
	if errors.As(err, &errno) {
		return fmt.Errorf("start pseudoterminal child: %w", errno)
	}
	return errors.New("start pseudoterminal child")
}
