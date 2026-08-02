package ai

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

// Caps on gathered context, per the privacy contract.
const (
	capStatus     = 1000
	capStagedDiff = 1500
	capHelp       = 600
	capGeneric    = 1000
	maxDirEntries = 30
	maxSubdirs    = 15
)

// helpAllowlist restricts --help gathering to common developer tools.
var helpAllowlist = map[string]bool{
	"git": true, "docker": true, "kubectl": true, "npm": true, "yarn": true,
	"pnpm": true, "cargo": true, "go": true, "systemctl": true, "helm": true,
	"terraform": true, "aws": true, "gcloud": true, "az": true, "make": true,
	"bun": true, "pip": true, "python": true, "python3": true, "node": true,
	"deno": true, "tar": true, "curl": true, "wget": true, "ssh": true,
	"podman": true, "tofu": true, "ansible": true, "gh": true, "nix": true,
}

// Snapshot is the bounded environment context sent with an AI request.
type Snapshot struct {
	CWD            string
	PrevCommand    string
	PrevExitStatus int
	RecentCommands []string // up to three
	Sections       []Section
}

// Section is one labeled block of untrusted context.
type Section struct {
	Label   string
	Content string
}

// Hash returns a stable digest of the snapshot for caching.
func (s Snapshot) Hash() string {
	var b strings.Builder
	fmt.Fprintf(&b, "%s|%s|%d|%s|", s.CWD, s.PrevCommand, s.PrevExitStatus, strings.Join(s.RecentCommands, ";"))
	for _, sec := range s.Sections {
		fmt.Fprintf(&b, "%s=%d;", sec.Label, len(sec.Content))
	}
	return b.String()
}

// Prober runs one bounded external probe; injected so context gathering
// reuses the generators' safe execution path.
type Prober func(timeout time.Duration, name string, args ...string) string

// Gatherer builds environment snapshots with a small TTL cache for
// dynamic provider context.
type Gatherer struct {
	Probe Prober

	mu    sync.Mutex
	cache map[string]cacheEntry
}

type cacheEntry struct {
	content string
	expires time.Time
}

const (
	contextTTL      = 4 * time.Second
	maxCacheEntries = 50
)

// cached memoizes one probe result for the context TTL.
func (g *Gatherer) cached(key string, fn func() string) string {
	g.mu.Lock()
	if e, ok := g.cache[key]; ok && time.Now().Before(e.expires) {
		g.mu.Unlock()
		return e.content
	}
	g.mu.Unlock()
	content := fn()
	g.mu.Lock()
	if g.cache == nil || len(g.cache) >= maxCacheEntries {
		g.cache = map[string]cacheEntry{}
	}
	g.cache[key] = cacheEntry{content: content, expires: time.Now().Add(contextTTL)}
	g.mu.Unlock()
	return content
}

// Gather builds the snapshot for the typed buffer, choosing the first
// matching specialized provider and otherwise the universal workspace
// provider.
func (g *Gatherer) Gather(cwd, buffer, prevCommand string, prevExit int, recent []string) Snapshot {
	snap := Snapshot{
		CWD:            cwd,
		PrevCommand:    prevCommand,
		PrevExitStatus: prevExit,
		RecentCommands: lastN(recent, 3),
	}
	if sec, ok := g.specialized(cwd, buffer); ok {
		snap.Sections = sec
		return snap
	}
	snap.Sections = g.universal(cwd, buffer)
	return snap
}

// specialized returns provider context for recognized command shapes.
func (g *Gatherer) specialized(cwd, buffer string) ([]Section, bool) {
	fields := strings.Fields(buffer)
	if len(fields) == 0 {
		return nil, false
	}
	probe := func(label, key string, timeout time.Duration, name string, args ...string) []Section {
		out := g.cached(key, func() string { return g.Probe(timeout, name, args...) })
		return []Section{{Label: label, Content: truncate(out, capGeneric)}}
	}
	head := fields[0]
	sub := ""
	if len(fields) > 1 {
		sub = fields[1]
	}
	switch {
	case head == "docker" && sub == "compose" && len(fields) > 2 && isOneOf(fields[2], "exec", "logs"),
		head == "docker-compose" && isOneOf(sub, "exec", "logs"):
		return probe("running compose services", "compose-ps", time.Second, "docker", "compose", "ps", "--format", "{{.Name}}\t{{.Status}}"), true
	case head == "docker" && isOneOf(sub, "exec", "logs", "stop", "restart", "rm"):
		sections := probe("running containers", "docker-ps", time.Second, "docker", "ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Status}}")
		images := g.cached("docker-images", func() string {
			return g.Probe(time.Second, "docker", "images", "--format", "{{.Repository}}:{{.Tag}}")
		})
		return append(sections, Section{Label: "local images", Content: truncate(images, capGeneric)}), true
	case head == "kubectl" && (isOneOf(sub, "exec", "logs") ||
		(len(fields) > 2 && isOneOf(sub, "describe", "delete") && strings.HasPrefix(fields[2], "pod"))):
		return probe("current pods", "kubectl-pods", time.Second, "kubectl", "get", "pods", "-o", "name"), true
	case head == "git" && isOneOf(sub, "checkout", "switch", "merge", "rebase") ||
		(head == "git" && sub == "branch" && strings.Contains(buffer, "-d")):
		return probe("local and remote branches", "git-branches", time.Second, "git", "branch", "-a", "--format=%(refname:short)"), true
	case head == "kill":
		return probe("top processes", "top-procs", time.Second, "ps", "-arxo", "pid=,pcpu=,pmem=,comm="), true
	case head == "systemctl" && isOneOf(sub, "restart", "stop", "status"):
		return probe("service units", "systemd-units", time.Second, "systemctl", "list-units", "--type=service", "--no-legend", "--no-pager"), true
	}
	return nil, false
}

// universal gathers ecosystem, script, directory, and Git context with
// sub-second budgets per probe.
func (g *Gatherer) universal(cwd, buffer string) []Section {
	var sections []Section
	if eco := detectEcosystems(cwd); eco != "" {
		sections = append(sections, Section{Label: "detected ecosystems", Content: eco})
	}
	if scripts := workspaceScripts(cwd); scripts != "" {
		sections = append(sections, Section{Label: "package scripts and tasks", Content: truncate(scripts, capGeneric)})
	}
	if listing := dirListing(cwd); listing != "" {
		sections = append(sections, Section{Label: "directory entries", Content: listing})
	}
	sections = append(sections, g.gitSections(cwd)...)
	if help := g.helpFor(buffer); help != "" {
		sections = append(sections, Section{Label: "command help", Content: help})
	}
	return sections
}

// gitSections gathers independent Git probes; each has a sub-second
// budget.
func (g *Gatherer) gitSections(cwd string) []Section {
	if _, err := os.Stat(filepath.Join(cwd, ".git")); err != nil {
		return nil
	}
	type result struct {
		label   string
		content string
		order   int
	}
	probes := []struct {
		label string
		limit int
		args  []string
	}{
		{"current branch", capGeneric, []string{"rev-parse", "--abbrev-ref", "HEAD"}},
		{"recent branches", capGeneric, []string{"branch", "--sort=-committerdate", "--format=%(refname:short)", "-l"}},
		{"git status", capStatus, []string{"status", "--short", "--branch"}},
		{"staged diff", capStagedDiff, []string{"diff", "--staged", "--stat"}},
		{"recent commits", capGeneric, []string{"log", "-5", "--format=%s"}},
	}
	results := make(chan result, len(probes))
	for i, p := range probes {
		go func() {
			out := g.cached("git-"+p.label, func() string {
				return g.Probe(900*time.Millisecond, "git", append([]string{"-C", cwd}, p.args...)...)
			})
			results <- result{label: p.label, content: truncate(out, p.limit), order: i}
		}()
	}
	collected := make([]result, 0, len(probes))
	for range probes {
		collected = append(collected, <-results)
	}
	sort.Slice(collected, func(i, j int) bool { return collected[i].order < collected[j].order })
	var sections []Section
	for _, r := range collected {
		if r.content != "" {
			sections = append(sections, Section{Label: r.label, Content: r.content})
		}
	}
	if _, err := os.Stat(filepath.Join(cwd, ".git", "MERGE_HEAD")); err == nil {
		sections = append(sections, Section{Label: "repository state", Content: "merge in progress"})
	}
	if _, err := os.Stat(filepath.Join(cwd, ".git", "rebase-merge")); err == nil {
		sections = append(sections, Section{Label: "repository state", Content: "rebase in progress"})
	}
	return sections
}

// helpFor gathers --help output for allowlisted tools only, rejecting
// names containing path separators.
func (g *Gatherer) helpFor(buffer string) string {
	fields := strings.Fields(buffer)
	if len(fields) == 0 {
		return ""
	}
	name := fields[0]
	if strings.ContainsAny(name, `/\`) || !helpAllowlist[name] {
		return ""
	}
	out := g.cached("help-"+name, func() string {
		return g.Probe(time.Second, name, "--help")
	})
	return truncate(out, capHelp)
}

func detectEcosystems(cwd string) string {
	markers := map[string]string{
		"package.json": "node", "go.mod": "go", "Cargo.toml": "rust",
		"pyproject.toml": "python", "requirements.txt": "python",
		"Dockerfile": "docker", "Makefile": "make", "justfile": "just",
		"Justfile": "just", ".git": "git",
	}
	var found []string
	for marker, name := range markers {
		if _, err := os.Stat(filepath.Join(cwd, marker)); err == nil {
			found = append(found, name)
		}
	}
	sort.Strings(found)
	return strings.Join(found, ", ")
}

// workspaceScripts extracts package scripts and Make/Just target names
// from the current directory and up to 15 visible immediate
// subdirectories, excluding node_modules.
func workspaceScripts(cwd string) string {
	var b strings.Builder
	appendDir := func(dir, prefix string) {
		for _, f := range []string{"package.json", "Makefile", "justfile", "Justfile"} {
			if _, err := os.Stat(filepath.Join(dir, f)); err == nil {
				fmt.Fprintf(&b, "%s%s\n", prefix, f)
			}
		}
	}
	appendDir(cwd, "")
	entries, err := os.ReadDir(cwd)
	if err != nil {
		return b.String()
	}
	count := 0
	for _, e := range entries {
		if !e.IsDir() || strings.HasPrefix(e.Name(), ".") || e.Name() == "node_modules" {
			continue
		}
		if count++; count > maxSubdirs {
			break
		}
		appendDir(filepath.Join(cwd, e.Name()), e.Name()+"/")
	}
	return b.String()
}

// dirListing lists at most 30 current-directory entries.
func dirListing(cwd string) string {
	entries, err := os.ReadDir(cwd)
	if err != nil {
		return ""
	}
	var names []string
	for _, e := range entries {
		if strings.HasPrefix(e.Name(), ".") {
			continue
		}
		name := e.Name()
		if e.IsDir() {
			name += "/"
		}
		names = append(names, name)
		if len(names) >= maxDirEntries {
			break
		}
	}
	return strings.Join(names, "\n")
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n]
}

func lastN(items []string, n int) []string {
	if len(items) <= n {
		return items
	}
	return items[len(items)-n:]
}

func isOneOf(s string, options ...string) bool {
	for _, o := range options {
		if s == o {
			return true
		}
	}
	return false
}
