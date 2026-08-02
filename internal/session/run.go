package session

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
	"github.com/rselbach/argmax/internal/engine"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/overlay"
	"github.com/rselbach/argmax/internal/shell"

	"github.com/creack/pty"
	"golang.org/x/sys/unix"
)

// sess holds the state of one wrapped-shell session. The event loop owns all
// UI state; other goroutines communicate through channels.
type sess struct {
	opts    Options
	version string
	cfg     *config.Config
	paths   config.Paths
	sh      shell.Shell
	keys    keymap

	tty       *tty
	ptmx      *os.File
	cmd       *exec.Cmd
	shellPgid int
	hookR     *os.File
	hookW     *os.File

	out   io.Writer // stdout for overlay + pump
	outMu sync.Mutex

	eng   *engine.Engine
	state *config.State

	// event channels
	inCh   chan []byte
	hookCh chan shell.Event
	exitCh chan error
	aiCh   chan aiResult
	sugCh  chan sugResult
	updCh  chan string
	sigCh  chan os.Signal

	// timers (created in loop)
	renderT *time.Timer
	aiT     *time.Timer
	emptyT  *time.Timer

	// UI state (loop-owned)
	buffer     []rune
	cursor     int // rune index
	atPrompt   bool
	cmdActive  bool
	cmdStart   time.Time // when cmdActive was last set (recovery grace window)
	sawHooks   bool      // any hook event observed (disables learning in pgrp recovery)
	pasting    bool
	pasteBuf   []rune
	mode       core.Mode
	menuOn     bool
	menuOpen   bool
	navigated  bool
	hiddenEdit bool // Esc: hidden until next eligible edit (IN-010)
	suggs      []core.Suggestion
	selected   int
	frame      overlay.Frame
	ghostOn    bool
	emptyPred  bool // empty-prompt prediction shown as ghost
	explicit   bool // menu opened via Up/Down

	// geometry
	termW, termH      int
	vt                *vtTracker // real cursor position, tracked from output
	bufferGen         uint64     // incremented on every buffer change
	lastSubmitted     string
	cfgModTime        time.Time
	updateNotice      string
	updateSeenVer     string
	startupModeIsLast bool
}

type keymap struct {
	toggleMode keyEvent
	toggleMenu keyEvent
	select_    keyEvent
	navUp      keyEvent
	navDown    keyEvent
}

func parseKeymap(cfg *config.Config) keymap {
	conv := func(name string) keyEvent {
		k, err := config.ParseKey(name)
		if err != nil {
			return keyEvent{kind: keyUnknown}
		}
		switch k.Kind {
		case config.KeyRune:
			return keyEvent{kind: keyRune, r: k.Rune}
		case config.KeyCtrl:
			return keyEvent{kind: keyCtrl, ctrl: k.Ctrl}
		default:
			return keyEvent{kind: keySpecial, spec: k.Special}
		}
	}
	return keymap{
		toggleMode: conv(cfg.Keybindings.ToggleMode),
		toggleMenu: conv(cfg.Keybindings.ToggleMenu),
		select_:    conv(cfg.Keybindings.Select),
		navUp:      conv(cfg.Keybindings.NavigateUp),
		navDown:    conv(cfg.Keybindings.NavigateDown),
	}
}

func sameKey(a, b keyEvent) bool {
	return a.kind == b.kind && a.r == b.r && a.ctrl == b.ctrl && a.spec == b.spec
}

// runSession executes the interactive session with crash recovery (DIAG).
func runSession(opts Options, version string) (code int) {
	s := &sess{opts: opts, version: version}
	defer func() {
		if r := recover(); r != nil {
			s.emergencyRestore()
			path, _ := logs.WriteCrashReport(s.paths.CrashesDir, version, fmt.Errorf("panic: %v", r), nil)
			fmt.Fprintf(os.Stderr, "\r\nargmax: internal error: %v\r\ncrash report written to %s\r\n", r, path)
			os.Exit(3) // watchdog: report written, start rescue shell
		}
	}()
	return s.run()
}

// emergencyRestore best-effort terminal restoration after a panic.
func (s *sess) emergencyRestore() {
	if s.tty != nil {
		s.tty.restore()
	}
}

func (s *sess) run() int {
	s.paths = config.ResolvePaths()
	if err := s.paths.EnsureDirs(); err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 1
	}
	_ = logs.Init(s.paths.LogFile, s.opts.Debug)
	defer logs.Close()
	logs.Info("session starting", "version", s.version, "pid", os.Getpid())

	cfg, err := config.LoadValid(s.paths.ConfigFile)
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax: invalid configuration: %v\n", err)
		return 1
	}
	if s.opts.Debug {
		cfg.Core.Debug = true
	}
	s.cfg = cfg
	s.keys = parseKeymap(cfg)
	s.cfgModTime = fileModTime(s.paths.ConfigFile)

	// RUN-010: refuse to nest inside a live argmax session.
	if marker := os.Getenv("ARGMAX_SESSION"); marker != "" {
		var pid int
		if _, err := fmt.Sscanf(marker, "%d", &pid); err == nil && pidAlive(pid) {
			fmt.Fprintf(os.Stderr, "argmax: already inside an argmax session (pid %d)\n", pid)
			return 1
		}
	}

	s.sh, err = shell.Detect(s.opts.ShellFlag, cfg.Core.Shell)
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 1
	}

	s.tty, err = openTTY()
	if err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 1
	}
	if err := s.tty.makeRaw(); err != nil {
		fmt.Fprintf(os.Stderr, "argmax: raw mode: %v\n", err)
		return 1
	}
	defer s.tty.restore()
	s.termW, s.termH = s.tty.size()
	s.vt = newVTTracker(s.termW, s.termH)

	cwd, _ := os.Getwd()
	if rc := os.Getenv("ARGMAX_RELOAD_CWD"); rc != "" && rc[0] == '/' {
		cwd = rc // RUN-013: reload retains the current directory
	}
	s.eng = engine.New(cfg, s.sh, cwd, s.paths)
	defer s.eng.Close()

	s.state, _ = config.LoadState(s.paths.StateFile)
	s.startupModeIsLast = cfg.Core.Mode == "last"
	s.mode = core.ModeSpec
	s.menuOn = true
	s.selected = -1
	s.atPrompt = true
	switch cfg.Core.Mode {
	case "history":
		s.mode = core.ModeHistory
	case "last":
		if m, ok := core.ParseMode(s.state.LastMode); ok {
			s.mode = m
		}
	}
	s.eng.SetMode(s.mode)

	if err := s.launchShell(cwd); err != nil {
		fmt.Fprintf(os.Stderr, "argmax: %v\n", err)
		return 1
	}

	// Background update check: asynchronous, never blocks the prompt (UPD-002).
	if cfg.Updater.CheckOnStartup && s.version != "dev" {
		go s.updateCheck()
	}

	code := s.loop()
	return code
}

// launchShell starts the child shell on a new PTY with the hook pipe (RUN-002).
func (s *sess) launchShell(cwd string) error {
	hookR, hookW, err := os.Pipe()
	if err != nil {
		return err
	}
	s.hookR, s.hookW = hookR, hookW

	exe := s.sh.Executable()
	args := s.sh.Args(s.cfg.Core.ShellLogin)
	s.cmd = exec.Command(exe, args...)
	s.cmd.Dir = cwd
	s.cmd.ExtraFiles = []*os.File{hookW} // child fd 3 (SH-006)

	env := []string{}
	for _, kv := range os.Environ() {
		if strings.HasPrefix(kv, "ARGMAX_SESSION=") || strings.HasPrefix(kv, "ARGMAX_RESCUE=") || strings.HasPrefix(kv, "ARGMAX_RELOAD_CWD=") {
			continue
		}
		env = append(env, kv)
	}
	env = append(env,
		fmt.Sprintf("ARGMAX_SESSION=%d", os.Getpid()),
		shell.HookFDEnv+"=3",
	)
	s.cmd.Env = env

	s.ptmx, err = pty.StartWithSize(s.cmd, s.tty.winsize())
	_ = hookW.Close() // parent only reads
	if err != nil {
		return fmt.Errorf("start shell: %w", err)
	}
	if pgid, err := unix.Getpgid(s.cmd.Process.Pid); err == nil {
		s.shellPgid = pgid
	}
	logs.Info("shell started", "shell", s.sh, "pid", s.cmd.Process.Pid)

	s.out = os.Stdout
	s.inCh = make(chan []byte, 16)
	s.hookCh = make(chan shell.Event, 32)
	s.exitCh = make(chan error, 1)
	s.aiCh = make(chan aiResult, 1)
	s.sugCh = make(chan sugResult, 1)
	s.updCh = make(chan string, 1)
	s.sigCh = make(chan os.Signal, 8)

	signal.Notify(s.sigCh, syscall.SIGWINCH, syscall.SIGUSR1, syscall.SIGTERM, syscall.SIGHUP, syscall.SIGQUIT)

	// The session file doubles as the clean-exit marker for the watchdog:
	// it is removed only by an orderly shutdown(). A panic, kill, or fatal
	// exit leaves it behind, which is how the watchdog detects a crash.
	s.writeSessionFile()

	go s.readInput()
	go s.readHooks()
	go s.pumpOutput()
	return nil
}

// readInput forwards raw terminal input chunks to the event loop.
func (s *sess) readInput() {
	buf := make([]byte, 4096)
	for {
		n, err := s.tty.file.Read(buf)
		if n > 0 {
			chunk := make([]byte, n)
			copy(chunk, buf[:n])
			select {
			case s.inCh <- chunk:
			case <-time.After(50 * time.Millisecond):
			}
		}
		if err != nil {
			select {
			case s.exitCh <- err:
			default:
			}
			return
		}
	}
}

// readHooks parses NUL-delimited hook events from fd 3 (SH-006).
func (s *sess) readHooks() {
	buf := make([]byte, 4096)
	var rest []byte
	for {
		n, err := s.hookR.Read(buf)
		if n > 0 {
			events, r := shell.ParseEvents(append(rest, buf[:n]...))
			rest = r
			for _, ev := range events {
				select {
				case s.hookCh <- ev:
				default:
				}
			}
		}
		if err != nil {
			return
		}
	}
}

// pumpOutput copies shell output to stdout, serialized with overlay writes
// (RUN-004), and feeds the cursor tracker. Expected EOF/EIO after shell exit
// ends the session (DIAG-005).
func (s *sess) pumpOutput() {
	buf := make([]byte, 16384)
	for {
		n, err := s.ptmx.Read(buf)
		if n > 0 {
			s.vt.feed(buf[:n])
			s.outMu.Lock()
			_, _ = s.out.Write(buf[:n])
			s.outMu.Unlock()
		}
		if err != nil {
			if err != io.EOF && !isEIO(err) {
				logs.Warn("pty read failed", "err", err)
			}
			select {
			case s.exitCh <- err:
			default:
			}
			return
		}
	}
}

func isEIO(err error) bool {
	if err == nil {
		return false
	}
	if errno, ok := err.(syscall.Errno); ok {
		return errno == syscall.EIO
	}
	if pe, ok := err.(*os.PathError); ok {
		return isEIO(pe.Err)
	}
	return strings.Contains(err.Error(), "input/output error")
}

func fileModTime(path string) time.Time {
	if st, err := os.Stat(path); err == nil {
		return st.ModTime()
	}
	return time.Time{}
}
