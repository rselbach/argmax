// Package session implements the PTY wrapper: shell launch, raw-mode input
// handling, hook IPC, overlay rendering, hot reload, and the crash watchdog
// (PRD 9.1, 9.3, 9.4, 9.19).
package session

// Options carries the launch-time CLI flags.
type Options struct {
	ShellFlag string // --shell/-s override
	Login     bool   // --shell-login
	Debug     bool   // --debug/-d
}

// RunWatchdog is the outer supervisor process (PRD 9.19): it spawns the
// interactive session child, captures bounded stderr, restores the terminal,
// writes a crash report, and starts a rescue shell on unexpected failure.
func RunWatchdog(opts Options, version string) int {
	return runWatchdog(opts, version)
}

// Run executes the interactive wrapped-shell session. It is normally invoked
// as the hidden `argmax __session` child of the watchdog.
func Run(opts Options, version string) int {
	return runSession(opts, version)
}
