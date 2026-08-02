// Package cli implements the argmax command surface: session launch,
// init, setup, config, reload, version, update, crash-log, and uninstall.
package cli

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"syscall"
	"time"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/logging"
	"github.com/rselbach/argmax/internal/paths"
	"github.com/rselbach/argmax/internal/session"
	"github.com/rselbach/argmax/internal/shell"
	"github.com/rselbach/argmax/internal/state"
	"github.com/rselbach/argmax/internal/update"
	"github.com/rselbach/argmax/internal/watchdog"
)

// Version is injected at build time; development builds report "dev".
var Version = "dev"

// Main dispatches the CLI and returns the process exit code.
func Main(args []string) int {
	if len(args) > 0 && args[0] == watchdog.SessionArg {
		return runSession(args[1:])
	}
	if len(args) == 0 {
		return watchdog.Run(Version)
	}
	switch args[0] {
	case "init":
		return cmdInit(args[1:])
	case "setup":
		return cmdSetup(args[1:])
	case "config":
		return cmdConfig(args[1:])
	case "reload":
		return cmdReload()
	case "version":
		fmt.Println("argmax " + Version)
		return 0
	case "update":
		return cmdUpdate()
	case "crash-log":
		return cmdCrashLog(args[1:])
	case "uninstall":
		return cmdUninstall()
	case "help", "--help", "-h":
		usage(os.Stdout)
		return 0
	default:
		if args[0][0] == '-' {
			// Flags without a subcommand start a session.
			return watchdog.Run(Version)
		}
		fmt.Fprintf(os.Stderr, "argmax: unknown command %q\n", args[0])
		usage(os.Stderr)
		return 1
	}
}

func usage(w *os.File) {
	_, _ = fmt.Fprint(w, `argmax — terminal-resident command completion

Usage:
  argmax [--shell <bash|zsh|fish>] [--shell-login] [--debug]
  argmax init <bash|zsh|fish>   Print sourceable shell integration
  argmax setup [shell]          Install autostart hooks and configuration
  argmax config init            Create the default configuration
  argmax config show            Print the resolved configuration
  argmax reload                 Reload configuration in the active session
  argmax update                 Check for and install a newer release
  argmax crash-log [--clear]    Show or clear crash reports
  argmax uninstall              Remove hooks, state, and binaries
  argmax version                Print the version
`)
}

// sessionFlags parses the session launch flags.
func sessionFlags(args []string) (shellFlag string, login, debug bool, err error) {
	fs := flag.NewFlagSet("argmax", flag.ContinueOnError)
	fs.StringVar(&shellFlag, "shell", "", "shell to wrap: bash, zsh, or fish")
	fs.StringVar(&shellFlag, "s", "", "shell to wrap (shorthand)")
	fs.BoolVar(&login, "shell-login", false, "start the shell as a login shell")
	fs.BoolVar(&debug, "debug", false, "enable debug diagnostics")
	fs.BoolVar(&debug, "d", false, "enable debug diagnostics (shorthand)")
	fs.SetOutput(os.Stderr)
	err = fs.Parse(args)
	return shellFlag, login, debug, err
}

// runSession is the watchdog-monitored interactive child.
func runSession(args []string) int {
	shellFlag, login, debug, err := sessionFlags(args)
	if err != nil {
		return 2
	}
	cfg, err := config.Load(config.Path())
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	if debug {
		cfg.Core.Debug = true
	}
	if err := logging.Setup(cfg.Core.Debug); err != nil {
		fmt.Fprintln(os.Stderr, "argmax: logging unavailable:", err)
	}
	defer logging.Close()
	if cfg.Core.Debug {
		logging.L().Warn("debug logging enabled: logs may contain everything typed at the prompt")
	}

	kind, err := shell.Detect(shellFlag, cfg.Core.Shell)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	loginFlag := login
	login = login || cfg.Core.ShellLogin

	watcher := config.NewWatcher(config.Path(), cfg)
	go checkForUpdates(cfg)
	code, err := session.Run(session.Options{
		Watcher:   watcher,
		Shell:     kind,
		Login:     login,
		ShellFlag: shellFlag,
		LoginFlag: loginFlag,
		Version:   Version,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		if code == 0 {
			code = 1
		}
	}
	return code
}

// checkForUpdates runs the asynchronous startup release check; failures
// are silent outside debug logs and never delay the first prompt.
func checkForUpdates(cfg *config.Config) {
	if !cfg.Updater.CheckOnStartup || Version == "dev" {
		return
	}
	st := state.Load()
	if time.Since(st.Updater.LastCheckTime) < cfg.CheckInterval() {
		return
	}
	rel, ok, err := update.Latest(cfg.Updater.Channel)
	st.Updater.LastCheckTime = time.Now().UTC()
	if err != nil {
		logging.L().Debug("update check failed", "error", err)
	} else if ok && update.IsNewer(Version, rel.Version) && st.Updater.SeenVersion != rel.Version {
		// Discovery is cached across sessions; each new version notifies
		// once, after a command completes, so the prompt stays stable.
		st.Updater.SeenVersion = rel.Version
		logging.L().Info("new version available", "version", rel.Version)
	}
	if err := state.Save(st); err != nil {
		logging.L().Debug("updater state save failed", "error", err)
	}
}

func cmdInit(args []string) int {
	if len(args) != 1 || !shell.Supported(args[0]) {
		fmt.Fprintln(os.Stderr, "argmax: init requires exactly one of: bash, zsh, fish")
		return 1
	}
	fmt.Print(shell.InitScript(shell.Kind(args[0])))
	return 0
}

func cmdConfig(args []string) int {
	if len(args) != 1 {
		fmt.Fprintln(os.Stderr, "argmax: config requires a subcommand: init or show")
		return 1
	}
	switch args[0] {
	case "init":
		return cmdConfigInit()
	case "show":
		return cmdConfigShow()
	default:
		fmt.Fprintf(os.Stderr, "argmax: unknown config subcommand %q\n", args[0])
		return 1
	}
}

func cmdConfigInit() int {
	path := config.Path()
	if _, err := os.Stat(path); err == nil {
		fmt.Printf("configuration already exists at %s\n", path)
		return 0
	}
	if err := paths.EnsureDir(filepath.Dir(path)); err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	if err := os.WriteFile(path, []byte(config.DefaultTemplate), 0o600); err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	fmt.Printf("created %s\n", path)
	return 0
}

func cmdConfigShow() int {
	cfg, err := config.Load(config.Path())
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	out, err := cfg.Redacted()
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	fmt.Print(out)
	return 0
}

// cmdReload validates the configuration, then signals the active parent
// session to apply it (its watcher also picks changes up within a second).
func cmdReload() int {
	if _, err := config.Load(config.Path()); err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	pidStr := os.Getenv("ARGMAX_SESSION_PID")
	if pidStr == "" {
		fmt.Println("configuration is valid; no active argmax session to signal")
		return 0
	}
	pid, err := strconv.Atoi(pidStr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax: invalid ARGMAX_SESSION_PID")
		return 1
	}
	if err := syscall.Kill(pid, syscall.SIGUSR1); err != nil {
		fmt.Fprintln(os.Stderr, "argmax: signal session:", err)
		return 1
	}
	fmt.Println("reload signaled to the active session")
	return 0
}

func cmdUpdate() int {
	cfg, err := config.Load(config.Path())
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	rel, ok, err := update.Latest(cfg.Updater.Channel)
	if err != nil {
		fmt.Fprintln(os.Stderr, "argmax:", err)
		return 1
	}
	if !ok {
		fmt.Printf("no releases found on the %s channel\n", cfg.Updater.Channel)
		return 0
	}
	fmt.Printf("current version: %s\nlatest %s release: %s\n", Version, cfg.Updater.Channel, rel.Version)
	if !update.IsNewer(Version, rel.Version) {
		fmt.Println("argmax is up to date")
		return 0
	}
	if err := update.Apply(rel); err != nil {
		fmt.Fprintln(os.Stderr, "argmax: update failed, current binary left intact:", err)
		return 1
	}
	fmt.Printf("updated to %s\n", rel.Version)
	return 0
}

func cmdCrashLog(args []string) int {
	clear := len(args) == 1 && args[0] == "--clear"
	dir := paths.CrashDir()
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) == 0 {
		if clear {
			fmt.Println("no crash reports to remove")
			return 0
		}
		fmt.Println("no crash reports found")
		return 0
	}
	if clear {
		if err := os.RemoveAll(dir); err != nil {
			fmt.Fprintln(os.Stderr, "argmax:", err)
			return 1
		}
		fmt.Printf("removed %d crash report(s)\n", len(entries))
		return 0
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	fmt.Println(filepath.Join(dir, names[len(names)-1]))
	return 0
}
