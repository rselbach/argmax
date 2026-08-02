package generators

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// SSHHosts reads user and system SSH configuration and returns concrete
// Host aliases, de-duplicated, omitting wildcard and negated patterns.
// Reused for ssh, scp, and rsync.
func SSHHosts() complete.Generator {
	return func(_ complete.Context, _ []string, prefix string) []complete.Candidate {
		var files []string
		if home, err := os.UserHomeDir(); err == nil {
			files = append(files, filepath.Join(home, ".ssh", "config"))
		}
		files = append(files, "/etc/ssh/ssh_config")
		seen := map[string]bool{}
		var out []complete.Candidate
		for _, f := range files {
			for _, host := range sshHostsFromFile(f) {
				if seen[host] || !hasFoldPrefix(host, prefix) {
					continue
				}
				seen[host] = true
				out = append(out, complete.Candidate{
					Title: host, Description: "ssh host", Icon: "ssh", Priority: 60,
				})
			}
		}
		return out
	}
}

func sshHostsFromFile(path string) []string {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer func() { _ = f.Close() }()
	var hosts []string
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		fields := strings.Fields(line)
		if len(fields) < 2 || !strings.EqualFold(fields[0], "Host") {
			continue
		}
		for _, h := range fields[1:] {
			if strings.ContainsAny(h, "*?!") {
				continue
			}
			hosts = append(hosts, h)
		}
	}
	return hosts
}

// EnvVars suggests environment variable names with truncated values for
// export, env, printenv, and unset. Credential-like values are redacted.
func EnvVars() complete.Generator {
	return func(_ complete.Context, _ []string, prefix string) []complete.Candidate {
		var out []complete.Candidate
		for _, kv := range os.Environ() {
			name, value, _ := strings.Cut(kv, "=")
			if !hasFoldPrefix(name, prefix) {
				continue
			}
			desc := value
			if credentialLike(name) {
				desc = "<redacted>"
			} else if len(desc) > 40 {
				desc = desc[:40] + "…"
			}
			out = append(out, complete.Candidate{
				Title: name, Description: desc, Icon: "env", Priority: 50,
			})
		}
		return out
	}
}

func credentialLike(name string) bool {
	n := strings.ToUpper(name)
	for _, marker := range []string{"KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH"} {
		if strings.Contains(n, marker) {
			return true
		}
	}
	return false
}

// Processes enumerates PID and command name for process-targeting commands
// such as kill and killall.
func Processes() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		var out []complete.Candidate
		for _, ln := range lines(run(ctx.CWD, defaultTimeout, "ps", "-axo", "pid=,comm=")) {
			fields := strings.Fields(ln)
			if len(fields) < 2 {
				continue
			}
			pid, comm := fields[0], filepath.Base(fields[1])
			if !hasFoldPrefix(pid, prefix) && !hasFoldPrefix(comm, prefix) {
				continue
			}
			out = append(out, complete.Candidate{
				Title: pid, Description: comm, Icon: "process", Priority: 40,
			})
		}
		return out
	}
}

// ChmodModes suggests common permission arguments as the first positional
// value, then executable or script-like files.
func ChmodModes() complete.Generator {
	modes := []string{
		"+x", "-x", "u+x", "a+x", "u+rw", "go-w", "a-w", "u+s", "g+s", "+t",
		"755", "644", "600", "700", "777", "666", "400", "444", "750", "640", "664",
	}
	filesGen := Files()
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		if len(args) == 0 {
			var out []complete.Candidate
			for _, m := range modes {
				if !strings.HasPrefix(m, prefix) {
					continue
				}
				out = append(out, complete.Candidate{
					Title: m, Description: "permission mode", Icon: "shield", Priority: 60,
				})
			}
			return out
		}
		return filesGen(ctx, args, prefix)
	}
}

// Zoxide merges local directory completion with zoxide query results: up to
// 20 recent directories for an empty query, fuzzy-matched up to 10 for a
// name query.
func Zoxide() complete.Generator {
	dirGen := Directories()
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		out := dirGen(ctx, args, prefix)
		queryArgs := []string{"query", "-l"}
		limit := 20
		if prefix != "" {
			queryArgs = append(queryArgs, strings.Fields(prefix)...)
			limit = 10
		}
		for i, dir := range lines(run(ctx.CWD, defaultTimeout, "zoxide", queryArgs...)) {
			if i >= limit {
				break
			}
			out = append(out, complete.Candidate{
				Title:       dir,
				Insert:      dir,
				Description: "recent directory",
				Icon:        "folder",
				Priority:    55,
				IsDirectory: true,
			})
		}
		return out
	}
}
