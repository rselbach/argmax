// Package shell implements shell detection (RUN-014, RUN-015), the
// dual-purpose integration scripts printed by `argmax init` (SH-001..005),
// the hook event protocol (SH-006), alias/history/rc file locations
// (SH-007, HIST-002), and idempotent setup/removal of the managed autostart
// block (SH-008, UN-001).
package shell

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
)

// Shell is one of the supported interactive shells (RUN-015).
type Shell string

const (
	Bash Shell = "bash"
	Zsh  Shell = "zsh"
	Fish Shell = "fish"
)

// markerEnv is exported by the autostart branch of the integration script
// just before it execs argmax, so the new process knows which shell it
// replaced (RUN-014).
const markerEnv = "ARGMAX_SHELL"

// Valid reports whether s is one of the supported shells.
func (s Shell) Valid() bool {
	switch s {
	case Bash, Zsh, Fish:
		return true
	}
	return false
}

// normalize maps a shell reference — bare name ("zsh"), path
// ("/usr/bin/fish"), or login argv[0] ("-bash") — to its canonical form.
func normalize(value string) Shell {
	v := strings.TrimSpace(value)
	if i := strings.LastIndexByte(v, '/'); i >= 0 {
		v = v[i+1:]
	}
	v = strings.TrimPrefix(v, "-")
	return Shell(strings.ToLower(v))
}

// Detect resolves the shell using the RUN-014 precedence: explicit CLI flag,
// resolved configuration value, ARGMAX_SHELL marker from an autostart hook,
// parent-process inspection, SHELL, then Bash fallback. Unsupported explicit
// values (flag, config, marker) return an error before any session starts
// (RUN-015); implicit fallbacks degrade to Bash.
func Detect(cliFlag, cfgValue string) (Shell, error) {
	explicit := []struct{ source, value string }{
		{"--shell flag", cliFlag},
		{"configuration", cfgValue},
		{markerEnv + " marker", os.Getenv(markerEnv)},
	}
	for _, e := range explicit {
		if e.value == "" {
			continue
		}
		s := normalize(e.value)
		if !s.Valid() {
			return "", fmt.Errorf("unsupported shell %q from %s: supported shells are bash, zsh, fish", e.value, e.source)
		}
		return s, nil
	}
	if s, ok := detectFromParent(os.Getppid(), parentLookup); ok {
		return s, nil
	}
	if s := normalize(os.Getenv("SHELL")); s.Valid() {
		return s, nil
	}
	return Bash, nil
}

// procInfo describes one process on the ancestor chain: its command name (or
// path) and its parent PID.
type procInfo struct {
	name string
	ppid int
}

// parentLookup resolves a PID to its procInfo. It is a package-level
// variable so tests can stub the OS lookup; production uses procInfoOf.
var parentLookup = procInfoOf

// detectFromParent walks the ancestor chain (bounded) looking for a
// supported shell. The lookup function is injected for testing.
func detectFromParent(pid int, lookup func(pid int) (procInfo, bool)) (Shell, bool) {
	for depth := 0; depth < 10 && pid > 1; depth++ {
		info, ok := lookup(pid)
		if !ok {
			return "", false
		}
		if s := normalize(info.name); s.Valid() {
			return s, true
		}
		pid = info.ppid
	}
	return "", false
}

// procInfoOf resolves a process's command name and parent PID, preferring
// /proc on Linux with a ps(1) fallback everywhere.
func procInfoOf(pid int) (procInfo, bool) {
	if runtime.GOOS == "linux" {
		if info, ok := procInfoFromProcFS(pid); ok {
			return info, true
		}
	}
	return procInfoFromPS(pid)
}

// procInfoFromProcFS parses /proc/<pid>/stat. The comm field may contain
// spaces and parentheses, so it is taken between the first '(' and the last
// ')'; the fields after it are "state ppid ...".
func procInfoFromProcFS(pid int) (procInfo, bool) {
	b, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if err != nil {
		return procInfo{}, false
	}
	s := string(b)
	open := strings.IndexByte(s, '(')
	close := strings.LastIndexByte(s, ')')
	if open < 0 || close <= open+1 || close+1 >= len(s) {
		return procInfo{}, false
	}
	fields := strings.Fields(s[close+1:])
	if len(fields) < 2 {
		return procInfo{}, false
	}
	ppid, err := strconv.Atoi(fields[1])
	if err != nil {
		return procInfo{}, false
	}
	return procInfo{name: s[open+1 : close], ppid: ppid}, true
}

// procInfoFromPS uses `ps -p <pid> -o comm= -o ppid=`, which works on Linux
// and macOS. comm may contain spaces, so the last field is taken as ppid and
// everything before it as the command.
func procInfoFromPS(pid int) (procInfo, bool) {
	out, err := exec.Command("ps", "-p", strconv.Itoa(pid), "-o", "comm=", "-o", "ppid=").Output()
	if err != nil {
		return procInfo{}, false
	}
	fields := strings.Fields(string(out))
	if len(fields) < 2 {
		return procInfo{}, false
	}
	ppid, err := strconv.Atoi(fields[len(fields)-1])
	if err != nil {
		return procInfo{}, false
	}
	return procInfo{name: strings.Join(fields[:len(fields)-1], " "), ppid: ppid}, true
}

// Executable resolves the shell binary: PATH lookup first, then the
// /bin/<name> fallback.
func (s Shell) Executable() string {
	if path, err := exec.LookPath(string(s)); err == nil {
		return path
	}
	return "/bin/" + string(s)
}

// Args returns the argv used to launch the wrapped shell (RUN-012 covers the
// login variant). Bash and Zsh always run interactively; Fish is interactive
// by default and only needs the login flag.
func (s Shell) Args(login bool) []string {
	switch s {
	case Bash:
		if login {
			return []string{"--login", "-i"}
		}
		return []string{"-i"}
	case Zsh:
		if login {
			return []string{"-l", "-i"}
		}
		return []string{"-i"}
	case Fish:
		if login {
			return []string{"--login"}
		}
		return []string{}
	}
	return nil
}
