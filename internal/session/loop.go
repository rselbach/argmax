package session

import (
	"fmt"
	"os"
	"syscall"
	"time"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/logs"
	"github.com/rselbach/argmax/internal/shell"

	"golang.org/x/sys/unix"
)

// loop is the session event loop: input keys, hook events, timers, signals.
// It returns the process exit code when the session ends.
func (s *sess) loop() int {
	parser := &keyParser{}
	s.renderT = time.NewTimer(time.Hour)
	s.renderT.Stop()
	s.aiT = time.NewTimer(time.Hour)
	s.aiT.Stop()
	s.emptyT = time.NewTimer(time.Hour)
	s.emptyT.Stop()
	escT := time.NewTimer(time.Hour)
	escT.Stop()
	cfgT := time.NewTicker(time.Second)
	defer cfgT.Stop()

	escPending := false

	for {
		select {
		case chunk := <-s.inCh:
			events := parser.feed(chunk)
			if escPending {
				escT.Stop()
				escPending = false
			}
			for _, ev := range events {
				s.dispatch(ev)
			}
			if len(parser.pending) > 0 {
				escPending = true
				escT.Reset(35 * time.Millisecond)
			}
		case <-escT.C:
			escPending = false
			for _, ev := range parser.flush() {
				s.dispatch(ev)
			}
		case ev := <-s.hookCh:
			s.onHookEvent(ev)
		case err := <-s.exitCh:
			logs.Info("pty closed", "err", err)
			return s.shutdown()
		case sig := <-s.sigCh:
			switch sig {
			case syscall.SIGWINCH:
				s.onResize()
			case syscall.SIGUSR1:
				s.reloadExec()
				// unreachable on success
			case syscall.SIGTERM, syscall.SIGHUP, syscall.SIGQUIT:
				return s.shutdown()
			}
		case <-s.renderT.C:
			s.requestSuggest(false)
		case res := <-s.sugCh:
			s.onSuggestResult(res)
		case <-s.aiT.C:
			s.requestAI()
		case res := <-s.aiCh:
			s.onAIResult(res)
		case <-s.emptyT.C:
			s.requestEmpty()
		case ver := <-s.updCh:
			s.updateSeenVer = ver
			s.updateNotice = fmt.Sprintf("argmax %s is available — run `argmax update`", ver)
		case <-cfgT.C:
			s.checkConfigReload()
		}
	}
}

// onResize propagates the new size to the child PTY immediately (RUN-008).
func (s *sess) onResize() {
	s.termW, s.termH = s.tty.size()
	s.vt.resize(s.termW, s.termH)
	w, h := s.tty.size()
	_ = unix.IoctlSetWinsize(int(s.ptmx.Fd()), unix.TIOCSWINSZ, &unix.Winsize{Row: uint16(h), Col: uint16(w)})
	if s.shellPgid > 0 {
		_ = unix.Kill(-s.shellPgid, syscall.SIGWINCH)
	}
	s.clearOverlay()
	s.requestSuggest(false)
}

// onHookEvent applies one shell hook event.
func (s *sess) onHookEvent(ev shell.Event) {
	s.sawHooks = true
	switch ev.Type {
	case "cwd":
		s.eng.SetCWD(ev.Payload)
	case "prompt":
		s.atPrompt = true
		s.cmdActive = false
		code := 0
		_, _ = fmt.Sscanf(ev.Payload, "%d", &code)
		s.eng.CommandFinished(code)
		s.buffer = nil
		s.cursor = 0
		s.menuOpen = false
		s.explicit = false
		s.navigated = false
		s.selected = -1
		s.clearOverlay()
		s.flushUpdateNotice()
		// Empty-prompt prediction (EMPTY-004: debounced).
		if s.cfg.AI.Enabled && s.cfg.AI.SuggestOnEmpty.Enabled && s.emptyT != nil {
			s.emptyT.Reset(time.Duration(orDefault(s.cfg.AI.SuggestOnEmpty.DebounceMs, 800)) * time.Millisecond)
		}
	case "preexec":
		s.atPrompt = false
		s.cmdActive = true
		s.cmdStart = time.Now()
		s.menuOpen = false
		s.clearOverlay()
		s.eng.CommandStarted(ev.Payload)
		s.lastSubmitted = ev.Payload
	case "postexec":
		// prompt event handles completion; nothing extra needed
	case "buffer":
		// Zsh is the buffer authority, but its events lag our local edits by
		// a few milliseconds. Apply every report, and when it changes the
		// model, invalidate in-flight suggestions and re-render so the menu
		// converges on the authoritative buffer (SH-003).
		if ev.Text == string(s.buffer) {
			return
		}
		s.buffer = []rune(ev.Text)
		s.cursor = len(s.buffer)
		if ev.Cursor >= 0 && ev.Cursor <= len(s.buffer) {
			s.cursor = ev.Cursor
		}
		s.bufferGen++
		s.scheduleRender()
	}
}

// flushUpdateNotice prints a pending update notification once, right after a
// command completes so the prompt is stable (UPD-005).
func (s *sess) flushUpdateNotice() {
	if s.updateNotice == "" {
		return
	}
	notice := s.updateNotice
	s.updateNotice = ""
	s.outMu.Lock()
	_, _ = s.out.Write([]byte(notice + "\r\n"))
	s.outMu.Unlock()
	s.updateSeenVer = notice
	go func() {
		st, err := config.LoadState(s.paths.StateFile)
		if err == nil {
			st.Updater.SeenVersion = s.updateSeenVer
			_ = config.SaveState(s.paths.StateFile, st)
		}
	}()
}

// checkConfigReload polls the config file for changes (CFG-005).
func (s *sess) checkConfigReload() {
	mt := fileModTime(s.paths.ConfigFile)
	if mt.IsZero() || mt.Equal(s.cfgModTime) {
		return
	}
	s.cfgModTime = mt
	cfg, err := config.LoadValid(s.paths.ConfigFile)
	if err != nil {
		// CFG-007: retain last valid config, log, keep running.
		logs.Warn("config reload failed; keeping last valid", "err", err)
		return
	}
	s.cfg = cfg
	s.keys = parseKeymap(cfg)
	s.eng.SetConfig(cfg)
	logs.Info("configuration reloaded")
}

// shutdown restores state and exits with the child shell's exit code.
// Removing the session file here is the clean-exit signal for the watchdog.
func (s *sess) shutdown() int {
	s.clearOverlaySync()
	s.tty.restore()
	s.removeSessionFile()
	if s.cmd != nil && s.cmd.ProcessState == nil {
		_ = s.cmd.Process.Kill()
	}
	code := 0
	if s.cmd != nil {
		_ = s.cmd.Wait()
		if s.cmd.ProcessState != nil {
			code = s.cmd.ProcessState.ExitCode()
		}
	}
	if code < 0 {
		code = 0
	}
	logs.Info("session ended", "code", code)
	return code
}

// reloadExec implements RUN-013: restore the terminal and replace this
// process, retaining launch arguments, the selected shell, and the CWD.
// State that cannot survive replacement (the inner shell process, its jobs
// and variables) is documented in the README/reload help text.
func (s *sess) reloadExec() {
	s.clearOverlaySync()
	s.tty.restore()
	exe, err := os.Executable()
	if err != nil {
		logs.Error("reload: resolve executable", "err", err)
		return
	}
	argv := []string{exe, "__session"}
	if s.opts.ShellFlag != "" {
		argv = append(argv, "--shell", s.opts.ShellFlag)
	}
	if s.opts.Login {
		argv = append(argv, "--shell-login")
	}
	if s.opts.Debug {
		argv = append(argv, "--debug")
	}
	env := append(os.Environ(), "ARGMAX_RELOAD_CWD="+s.eng.CWD())
	if err := unix.Exec(exe, argv, env); err != nil {
		logs.Error("reload: exec failed", "err", err)
	}
}

func orDefault(v, def int) int {
	if v <= 0 {
		return def
	}
	return v
}
