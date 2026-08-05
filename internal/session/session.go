// Package session implements the interactive PTY session: shell launch,
// raw mode, input/output pumps, shell-event IPC, suggestion orchestration,
// and overlay rendering.
package session

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/creack/pty"
	"golang.org/x/sys/unix"
	"golang.org/x/term"

	"github.com/rselbach/argmax/internal/ai"
	"github.com/rselbach/argmax/internal/catalog"
	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/history"
	"github.com/rselbach/argmax/internal/infer"
	"github.com/rselbach/argmax/internal/input"
	"github.com/rselbach/argmax/internal/keymap"
	"github.com/rselbach/argmax/internal/logging"
	"github.com/rselbach/argmax/internal/rank"
	"github.com/rselbach/argmax/internal/shell"
	"github.com/rselbach/argmax/internal/state"
	"github.com/rselbach/argmax/internal/ui"
)

// Options configure a session launch.
type Options struct {
	Watcher *config.Watcher
	Shell   shell.Kind
	Login   bool
	// ShellFlag and LoginFlag preserve the explicit CLI selections so a
	// reload can re-resolve the effective shell with the same precedence.
	ShellFlag string
	LoginFlag bool
	// Version is the running product version, used for update notices.
	Version string
}

// mode is the active suggestion mode.
type mode string

const (
	modeSpec    mode = "spec"
	modeHistory mode = "history"
)

// Session wraps one interactive child shell in a PTY.
type Session struct {
	opts     Options
	tty      *os.File // controlling terminal input
	ttyState *term.State
	ptmx     *os.File
	child    *exec.Cmd
	eventsR  *os.File
	renderer *ui.Renderer
	registry *complete.Registry
	engine   *complete.Engine
	inferrer *infer.Inferrer
	aliases  *shell.AliasCache
	hist     *history.Provider
	store    *rank.Store
	detector *rank.Detector
	aiEngine *ai.Engine
	gatherer *ai.Gatherer

	keys keybindings

	mu            sync.Mutex
	buf           input.Buffer
	cwd           string
	commandActive bool
	// hooksSeen records that shell integration events arrived; until
	// then, watchForeground provides command-boundary detection.
	hooksSeen     bool
	menuEnabled   bool
	menuVisible   bool
	suppressed    bool
	pasting       bool
	navigated     bool
	items         []complete.Candidate
	selected      int
	scroll        int
	mode          mode
	prevCommand   string
	prevExit      int
	prevSkeleton  string
	lastSubmitted string
	updateNotice  string
	noticeShown   bool
	generation    uint64
	renderPending bool
	// renderSeq records the buffer version a pending cursor query was
	// issued for; the render is dropped when edits arrived meanwhile, so
	// a stale erase can never blank freshly echoed text.
	renderSeq uint64
	// dsrSeq records the renderer output sequence when the cursor query
	// was issued; shell output racing the query invalidates the report
	// and triggers a re-query.
	dsrSeq     uint64
	dsrRetries int
	// dsrOutstanding counts our unanswered cursor queries. A report that
	// arrives with none outstanding belongs to the shell or a TUI (some
	// prompt themes issue their own queries) and is forwarded intact.
	dsrOutstanding int

	coalesce    *time.Timer
	emptyTimer  *time.Timer
	noticeTimer *time.Timer
	done        chan struct{}
	doneOnce    sync.Once
	ctx         context.Context
	cancel      context.CancelFunc

	workerMu sync.Mutex
	workers  sync.WaitGroup
	stopping bool
}

// keybindings holds the resolved configurable keys.
type keybindings struct {
	toggleMode keymap.Key
	toggleMenu keymap.Key
	selectKey  keymap.Key
	navUp      keymap.Key
	navDown    keymap.Key
}

// Run starts the wrapped shell and blocks until it exits, returning the
// shell's exit code.
func Run(opts Options) (int, error) {
	// Catalog construction decompresses the embedded data bundle; overlap
	// it with the child shell's own startup.
	registryCh := make(chan *complete.Registry, 1)
	go func() { registryCh <- catalog.Registry() }()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s := &Session{
		opts:        opts,
		inferrer:    infer.New(),
		aliases:     shell.NewAliasCache(opts.Shell),
		detector:    &rank.Detector{},
		menuEnabled: true,
		selected:    -1,
		done:        make(chan struct{}),
		ctx:         ctx,
		cancel:      cancel,
	}
	s.hist = history.NewProvider(opts.Shell.HistoryPath(), historyFormat(opts.Shell))
	s.mode = startupMode(opts.Watcher.Current())
	if cwd, err := os.Getwd(); err == nil {
		s.cwd = cwd
	}

	store, err := rank.Open()
	if err != nil {
		logging.L().Warn("learning database unavailable, using static ranking", "error", err)
	}
	s.store = store
	defer func() { _ = s.store.Close() }()

	s.gatherer = &ai.Gatherer{Probe: probe}
	s.aiEngine = &ai.Engine{Gatherer: s.gatherer, Log: func(msg string, err error) {
		logging.L().Debug(msg, "error", err)
	}}
	s.configure(opts.Watcher.Current())

	if err := s.start(); err != nil {
		return 1, err
	}
	// The shell is sourcing its rc files; the registry is ready before
	// any pump that uses it starts.
	s.registry = <-registryCh
	s.engine = &complete.Engine{Registry: s.registry}
	return s.serve()
}

func historyFormat(k shell.Kind) history.Format {
	switch k {
	case shell.Zsh:
		return history.FormatZsh
	case shell.Fish:
		return history.FormatFish
	default:
		return history.FormatBash
	}
}

func startupMode(cfg *config.Config) mode {
	switch cfg.Core.Mode {
	case "spec":
		return modeSpec
	case "history":
		return modeHistory
	default:
		if state.Load().LastMode == "history" {
			return modeHistory
		}
		return modeSpec
	}
}

// configure applies (re)loaded configuration to live components.
func (s *Session) configure(cfg *config.Config) {
	parse := func(name string) keymap.Key {
		k, err := keymap.Parse(name)
		if err != nil {
			return keymap.Key{Kind: keymap.KindUnknown}
		}
		return k
	}
	s.mu.Lock()
	s.keys = keybindings{
		toggleMode: parse(cfg.Keybindings.ToggleMode),
		toggleMenu: parse(cfg.Keybindings.ToggleMenu),
		selectKey:  parse(cfg.Keybindings.Select),
		navUp:      parse(cfg.Keybindings.NavigateUp),
		navDown:    parse(cfg.Keybindings.NavigateDown),
	}
	s.mu.Unlock()
	if s.renderer != nil {
		s.renderer.SetOptions(s.uiOptions(cfg))
	}
	s.aiEngine.Configure(cfg.AI, cfg.ActiveProvider())
}

func (s *Session) uiOptions(cfg *config.Config) ui.Options {
	return ui.Options{
		Classic:    cfg.UI.Style == "classic",
		NerdFonts:  cfg.UI.NerdFonts,
		GhostText:  cfg.UI.GhostText,
		MaxHeight:  cfg.UI.MaxHeight,
		MaxWidth:   cfg.UI.MaxWidth,
		Palette:    ui.DefaultPalette,
		FooterHint: fmt.Sprintf("%s insert · %s mode · %s hide", cfg.Keybindings.Select, cfg.Keybindings.ToggleMode, cfg.Keybindings.ToggleMenu),
	}
}

// start launches the child shell attached to a new PTY.
func (s *Session) start() error {
	shellPath, err := s.opts.Shell.Path()
	if err != nil {
		return err
	}
	// Session-private event channel inherited by the shell as fd 3.
	eventsR, eventsW, err := os.Pipe()
	if err != nil {
		return fmt.Errorf("create event pipe: %w", err)
	}
	s.eventsR = eventsR

	// Work when stdin is redirected by opening the controlling terminal.
	s.tty = os.Stdin
	if !term.IsTerminal(int(os.Stdin.Fd())) {
		tty, err := os.OpenFile("/dev/tty", os.O_RDWR, 0)
		if err != nil {
			_ = eventsR.Close()
			_ = eventsW.Close()
			return fmt.Errorf("stdin is not a terminal and /dev/tty is unavailable: %w", err)
		}
		s.tty = tty
	}

	ptmx, tts, err := pty.Open()
	if err != nil {
		_ = eventsR.Close()
		_ = eventsW.Close()
		if s.tty != os.Stdin {
			_ = s.tty.Close()
		}
		return fmt.Errorf("open pty: %w", err)
	}
	if ws, err := pty.GetsizeFull(s.tty); err == nil {
		_ = pty.Setsize(ptmx, ws)
	}

	var args []string
	if s.opts.Login {
		args = append(args, "-l")
	}
	cmd := exec.Command(shellPath, args...)
	// ARGMAX_TTY lets hooks detect session markers inherited across a
	// tmux or SSH boundary.
	cmd.Env = append(os.Environ(),
		"ARGMAX_ACTIVE=1",
		"ARGMAX_SHELL="+string(s.opts.Shell),
		"ARGMAX_EVENTS_FD=3",
		"ARGMAX_TTY="+tts.Name(),
		fmt.Sprintf("ARGMAX_SESSION_PID=%d", os.Getpid()),
	)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = tts, tts, tts
	cmd.ExtraFiles = []*os.File{eventsW}
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true, Setctty: true}
	if err := cmd.Start(); err != nil {
		_ = ptmx.Close()
		_ = tts.Close()
		_ = eventsR.Close()
		_ = eventsW.Close()
		if s.tty != os.Stdin {
			_ = s.tty.Close()
		}
		return fmt.Errorf("start shell: %w", err)
	}
	_ = tts.Close()     // child holds the slave side
	_ = eventsW.Close() // child holds the write end
	s.ptmx = ptmx
	s.child = cmd

	s.ttyState, err = term.MakeRaw(int(s.tty.Fd()))
	if err != nil {
		_ = ptmx.Close()
		_ = eventsR.Close()
		if s.tty != os.Stdin {
			_ = s.tty.Close()
		}
		killAndWait(cmd)
		return fmt.Errorf("set raw mode: %w", err)
	}
	s.renderer = ui.NewRenderer(os.Stdout, s.uiOptions(s.opts.Watcher.Current()))
	if ws, err := pty.GetsizeFull(s.tty); err == nil {
		s.renderer.SetSize(int(ws.Cols), int(ws.Rows))
	}
	return nil
}

// serve runs the pumps until the shell exits.
func (s *Session) serve() (int, error) {
	defer s.shutdown()

	s.opts.Watcher.OnChange = func(cfg *config.Config) {
		s.configure(cfg)
		logging.L().Info("configuration reloaded")
	}
	s.opts.Watcher.OnError = func(err error) {
		logging.L().Warn("configuration reload failed; keeping last valid configuration", "error", err)
	}
	s.launchWorker(s.outputPump)
	s.launchWorker(s.inputPump)
	s.launchWorker(s.eventPump)
	s.launchWorker(s.watchForeground)
	s.launchWorker(s.watchResize)
	s.launchWorker(s.watchUpdateNotice)
	s.launchWorker(func() { s.opts.Watcher.Run(s.ctx) })

	err := s.child.Wait()
	s.stop()
	var exitErr *exec.ExitError
	if errors.As(err, &exitErr) {
		return exitErr.ExitCode(), nil
	}
	if err != nil {
		return 1, fmt.Errorf("shell exited: %w", err)
	}
	return 0, nil
}

func (s *Session) launchWorker(fn func()) bool {
	s.workerMu.Lock()
	defer s.workerMu.Unlock()
	if s.stopping {
		return false
	}
	if s.ctx != nil {
		select {
		case <-s.ctx.Done():
			return false
		default:
		}
	}
	s.workers.Add(1)
	go func() {
		defer s.workers.Done()
		fn()
	}()
	return true
}

func (s *Session) stop() {
	s.doneOnce.Do(func() { close(s.done) })
	if s.cancel != nil {
		s.cancel()
	}
}

func (s *Session) shutdown() {
	s.workerMu.Lock()
	s.stopping = true
	s.workerMu.Unlock()
	s.stop()

	s.mu.Lock()
	for _, timer := range []*time.Timer{s.coalesce, s.emptyTimer, s.noticeTimer} {
		if timer != nil {
			timer.Stop()
		}
	}
	s.coalesce = nil
	s.emptyTimer = nil
	s.noticeTimer = nil
	s.mu.Unlock()
	if s.aiEngine != nil {
		s.aiEngine.Cancel()
	}
	// Unblock the PTY and hook readers before joining their workers. The
	// input worker polls done and exits without closing the user's stdin.
	if s.ptmx != nil {
		_ = s.ptmx.Close()
	}
	if s.eventsR != nil {
		_ = s.eventsR.Close()
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	workersDone := make(chan struct{})
	go func() {
		s.workers.Wait()
		close(workersDone)
	}()
	select {
	case <-workersDone:
	case <-ctx.Done():
		logging.L().Warn("session workers did not stop before shutdown deadline")
	}
	if s.aiEngine != nil {
		if err := s.aiEngine.Wait(ctx); err != nil {
			logging.L().Warn("AI workers did not stop before shutdown deadline", "error", err)
		}
	}
	s.restore()
}

func (s *Session) restore() {
	if s.ttyState != nil && s.tty != nil {
		_ = term.Restore(int(s.tty.Fd()), s.ttyState)
		s.ttyState = nil
	}
	if s.ptmx != nil {
		_ = s.ptmx.Close()
		s.ptmx = nil
	}
	if s.eventsR != nil {
		_ = s.eventsR.Close()
		s.eventsR = nil
	}
	if s.tty != nil && s.tty != os.Stdin {
		_ = s.tty.Close()
		s.tty = nil
	}
}

func killAndWait(cmd *exec.Cmd) {
	if cmd == nil || cmd.Process == nil {
		return
	}
	_ = cmd.Process.Kill()
	_ = cmd.Wait()
}

// outputPump forwards shell output to stdout through the renderer so
// overlay and output writes are serialized. Expected EOF/EIO after shell
// exit is normal.
func (s *Session) outputPump() {
	buf := make([]byte, 32*1024)
	for {
		n, err := s.ptmx.Read(buf)
		if n > 0 {
			_, _ = s.renderer.Write(buf[:n])
		}
		if err != nil {
			if !errors.Is(err, io.EOF) && !errors.Is(err, syscall.EIO) && !errors.Is(err, os.ErrClosed) {
				logging.L().Warn("unexpected pty read failure", "error", err)
			}
			s.stop()
			return
		}
	}
}

// inputPump reads terminal input, decoding at the prompt and forwarding
// transparently while a foreground command is active.
func (s *Session) inputPump() {
	const loneEscapeTimeout = 50 * time.Millisecond

	dec := &input.Decoder{}
	buf := make([]byte, 4096)
	var (
		decodeMu        sync.Mutex
		escapeTimer     *time.Timer
		timerGeneration uint64
	)
	stopEscapeTimer := func() {
		timerGeneration++
		if escapeTimer != nil {
			escapeTimer.Stop()
			escapeTimer = nil
		}
	}
	defer func() {
		decodeMu.Lock()
		stopEscapeTimer()
		decodeMu.Unlock()
	}()
	dispatch := func(events []input.Event) {
		for _, ev := range events {
			s.handleEvent(ev)
		}
	}
	armEscapeTimer := func() {
		timerGeneration++
		generation := timerGeneration
		escapeTimer = time.AfterFunc(loneEscapeTimeout, func() {
			decodeMu.Lock()
			defer decodeMu.Unlock()
			if generation != timerGeneration {
				return
			}
			escapeTimer = nil
			select {
			case <-s.done:
				return
			default:
			}
			dispatch(dec.FlushPending())
		})
	}
	for {
		select {
		case <-s.done:
			return
		default:
		}
		poll := []unix.PollFd{{Fd: int32(s.tty.Fd()), Events: unix.POLLIN}}
		if _, err := unix.Poll(poll, 100); err != nil {
			if errors.Is(err, syscall.EINTR) {
				continue
			}
			logging.L().Debug("terminal input poll failed", "error", err)
			return
		}
		if poll[0].Revents == 0 {
			continue
		}
		n, err := s.tty.Read(buf)
		decodeMu.Lock()
		stopEscapeTimer()
		if n > 0 {
			dispatch(dec.Feed(buf[:n]))
			if dec.LoneEscapePending() {
				armEscapeTimer()
			}
		}
		decodeMu.Unlock()
		if err != nil {
			return
		}
		select {
		case <-s.done:
			return
		default:
		}
	}
}

// watchResize propagates terminal size changes to the child PTY
// immediately.
func (s *Session) watchResize() {
	ch := make(chan os.Signal, 1)
	signal.Notify(ch, syscall.SIGWINCH, syscall.SIGUSR1)
	defer signal.Stop(ch)
	for {
		select {
		case <-s.done:
			return
		case sig := <-ch:
			switch sig {
			case syscall.SIGWINCH:
				if ws, err := pty.GetsizeFull(s.tty); err == nil {
					_ = pty.Setsize(s.ptmx, ws)
					s.renderer.SetSize(int(ws.Cols), int(ws.Rows))
				}
			case syscall.SIGUSR1:
				s.handleReloadSignal()
			}
		}
	}
}

// handleReloadSignal applies a requested reload. Process replacement is
// heavyweight (session-local history and learning context restart), so it
// happens only when the effective shell selection changed; otherwise the
// new configuration is applied live.
func (s *Session) handleReloadSignal() {
	cfg, err := config.Load(config.Path())
	if err != nil {
		// A malformed reload keeps the last valid configuration.
		logging.L().Warn("reload requested with invalid configuration; keeping current settings", "error", err)
		return
	}
	kind, err := shell.Detect(s.opts.ShellFlag, cfg.Core.Shell)
	if err != nil {
		logging.L().Warn("reload requested with unusable shell selection", "error", err)
		return
	}
	login := s.opts.LoginFlag || cfg.Core.ShellLogin
	if kind == s.opts.Shell && login == s.opts.Login {
		logging.L().Info("reload: applying configuration live; shell selection unchanged")
		s.opts.Watcher.Refresh()
		return
	}
	logging.L().Info("reload: shell selection changed; replacing session process",
		"from", s.opts.Shell, "to", kind)
	s.reloadInPlace()
}

// reloadInPlace replaces the current process, retaining launch arguments,
// the selected shell, and the child's current directory. Shell-local state
// (session history, unsaved shell variables) does not survive replacement.
func (s *Session) reloadInPlace() {
	self, err := os.Executable()
	if err != nil {
		logging.L().Error("reload failed to resolve executable", "error", err)
		return
	}
	s.mu.Lock()
	cwd := s.cwd
	s.mu.Unlock()
	if cwd != "" {
		_ = os.Chdir(cwd)
	}
	s.restore()
	logging.L().Info("replacing session process for reload")
	logging.Close()
	if err := syscall.Exec(self, os.Args, os.Environ()); err != nil {
		logging.L().Error("process replacement failed", "error", err)
	}
}

// probe runs one bounded external command for AI context gathering.
func probe(cwd string, timeout time.Duration, name string, args ...string) string {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = cwd
	out, err := cmd.Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}
