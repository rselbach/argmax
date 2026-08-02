package ai

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"
)

// Context budgets and caps (PRD 9.12 universal context, PERF-005, PERF-007).
const (
	contextCacheTTL = 4 * time.Second // AI-012
	contextCacheMax = 50              // PERF-007

	specializedBudget = 1 * time.Second         // specialized provider commands
	universalBudget   = 1200 * time.Millisecond // universal workspace provider
	gitProbeBudget    = 900 * time.Millisecond  // per independent git probe
	helpBudget        = 900 * time.Millisecond

	maxDirEntries    = 30
	maxGitBranches   = 10
	maxRecentCommits = 5
	maxScripts       = 50
	maxStatusChars   = 1000
	maxStagedChars   = 1500
	maxSpecialChars  = 1000
	maxHelpChars     = 600

	maxPackageJSONBytes = 1 << 18 // 256 KiB guard for package.json
	maxBuildFileBytes   = 1 << 18 // 256 KiB guard for Makefile/justfile
)

// Context is the bounded environment snapshot sent to the provider (AI-007).
// Every field is empty when its probe failed or did not apply; failures never
// produce errors.
type Context struct {
	Ecosystems    []string
	Scripts       []string // "name: command"
	DirEntries    []string // ≤30
	GitBranch     string
	GitPrevBranch string
	GitBranches   []string // ≤10
	GitStatus     string   // ≤1000 chars
	StagedDiff    string   // ≤1500 chars
	RecentCommits []string // ≤5 subjects
	MergeState    string   // "merging"|"rebasing"|""
	Specialized   string   // output of the matched specialized provider, ≤1000 chars
	Help          string   // bounded --help output for allowlisted root cmd, ≤600 chars
}

// Specialized provider names, chosen by the typed buffer root+subcommand.
const (
	providerUniversal   = "universal"
	providerDocker      = "docker-containers"
	providerCompose     = "compose-services"
	providerPods        = "kubectl-pods"
	providerGitBranches = "git-branches"
	providerProcesses   = "processes"
	providerSystemd     = "systemd-services"
)

// selectProvider picks the FIRST matching specialized provider, otherwise the
// universal workspace provider.
func selectProvider(tokens []string) string {
	if len(tokens) == 0 {
		return providerUniversal
	}
	sub := func(i int) string {
		if i < len(tokens) {
			return tokens[i]
		}
		return ""
	}
	switch tokens[0] {
	case "docker":
		switch sub(1) {
		case "exec", "logs", "stop", "restart", "rm":
			return providerDocker
		case "compose":
			if sub(2) == "exec" || sub(2) == "logs" {
				return providerCompose
			}
		}
	case "docker-compose":
		if sub(1) == "exec" || sub(1) == "logs" {
			return providerCompose
		}
	case "kubectl":
		switch sub(1) {
		case "exec", "logs":
			return providerPods
		case "describe", "delete":
			if sub(2) == "pod" || sub(2) == "pods" {
				return providerPods
			}
		}
	case "git":
		switch sub(1) {
		case "checkout", "switch", "merge", "rebase":
			return providerGitBranches
		case "branch":
			if sub(2) == "-d" || sub(2) == "-D" {
				return providerGitBranches
			}
		}
	case "kill":
		return providerProcesses
	case "systemctl":
		switch sub(1) {
		case "restart", "stop", "status":
			return providerSystemd
		}
	}
	return providerUniversal
}

// contextCacheEntry is one cached snapshot. The provider is recorded so a
// cache-key collision between buffers mapping to different providers (e.g.
// "docker exec" vs "docker compose logs") never serves the wrong context.
type contextCacheEntry struct {
	provider string
	at       time.Time
	value    Context
}

// contextCache is the package-level 4s/50-entry context cache (AI-012,
// PERF-007).
var contextCache = struct {
	sync.Mutex
	entries map[string]contextCacheEntry
}{entries: make(map[string]contextCacheEntry)}

func contextCacheGet(key, provider string) (Context, bool) {
	contextCache.Lock()
	defer contextCache.Unlock()
	e, ok := contextCache.entries[key]
	if !ok || e.provider != provider || time.Since(e.at) >= contextCacheTTL {
		return Context{}, false
	}
	return e.value, true
}

func contextCacheSet(key, provider string, value Context) {
	contextCache.Lock()
	defer contextCache.Unlock()
	now := time.Now()
	if len(contextCache.entries) >= contextCacheMax {
		for k, e := range contextCache.entries {
			if now.Sub(e.at) >= contextCacheTTL {
				delete(contextCache.entries, k)
			}
		}
	}
	if len(contextCache.entries) >= contextCacheMax {
		var oldestKey string
		var oldest time.Time
		for k, e := range contextCache.entries {
			if oldestKey == "" || e.at.Before(oldest) {
				oldestKey, oldest = k, e.at
			}
		}
		delete(contextCache.entries, oldestKey)
	}
	contextCache.entries[key] = contextCacheEntry{provider: provider, at: now, value: value}
}

// resetContextCache empties the context cache. Used by tests.
func resetContextCache() {
	contextCache.Lock()
	defer contextCache.Unlock()
	contextCache.entries = make(map[string]contextCacheEntry)
}

// GatherContext builds the bounded environment snapshot (AI-007) choosing the
// first matching specialized provider, else the universal workspace provider.
// Specialized commands get a 1s budget; the universal provider 1.2s;
// independent git probes run concurrently with sub-second budgets. Results are
// cached 4s with ≤50 entries keyed by (buffer-root + cwd) (AI-012, PERF-007).
func GatherContext(ctx context.Context, req Request) Context {
	tokens := strings.Fields(req.Buffer)
	provider := selectProvider(tokens)
	root := ""
	if len(tokens) > 0 {
		root = tokens[0]
	}
	key := root + "\x00" + req.CWD
	if cached, ok := contextCacheGet(key, provider); ok {
		return cached
	}

	var c Context
	if provider == providerUniversal {
		c = gatherUniversal(ctx, req, tokens)
	} else {
		c.Specialized = gatherSpecialized(ctx, provider, req.CWD)
	}
	contextCacheSet(key, provider, c)
	return c
}

// gatherSpecialized runs the matched specialized provider with a 1s budget
// and caps its output at 1000 chars.
func gatherSpecialized(ctx context.Context, provider, dir string) string {
	ctx, cancel := context.WithTimeout(ctx, specializedBudget)
	defer cancel()

	var out string
	switch provider {
	case providerDocker:
		out = dockerResources(ctx, dir)
	case providerCompose:
		out = composeServices(ctx, dir)
	case providerPods:
		out, _ = runProbe(ctx, dir, maxSpecialChars, "kubectl", "get", "pods", "--no-headers")
	case providerGitBranches:
		out, _ = runProbe(ctx, dir, maxSpecialChars, "git", "branch", "-a")
	case providerProcesses:
		out = topProcesses(ctx, dir)
	case providerSystemd:
		out = systemdServices(ctx, dir)
	}
	return capString(out, maxSpecialChars)
}

// dockerResources lists running containers and images.
func dockerResources(ctx context.Context, dir string) string {
	var ps, images string
	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		if out, err := runProbe(ctx, dir, 600, "docker", "ps"); err == nil {
			ps = out
		}
	}()
	go func() {
		defer wg.Done()
		if out, err := runProbe(ctx, dir, 400, "docker", "images", "--format", "{{.Repository}}:{{.Tag}}"); err == nil {
			images = out
		}
	}()
	wg.Wait()

	var b strings.Builder
	if ps != "" {
		b.WriteString("containers:\n")
		b.WriteString(ps)
	}
	if images != "" {
		if b.Len() > 0 {
			b.WriteByte('\n')
		}
		b.WriteString("images:\n")
		b.WriteString(images)
	}
	return b.String()
}

// composeServices lists compose services, preferring the plugin form and
// falling back to the legacy docker-compose binary.
func composeServices(ctx context.Context, dir string) string {
	out, err := runProbe(ctx, dir, maxSpecialChars, "docker", "compose", "ps", "--services")
	if err != nil {
		out, err = runProbe(ctx, dir, maxSpecialChars, "docker-compose", "ps", "--services")
	}
	if err != nil {
		return ""
	}
	return out
}

// topProcesses lists the top processes with PID, CPU, and memory columns.
// The -r sort flag is not portable, so fall back to a plain listing.
func topProcesses(ctx context.Context, dir string) string {
	out, err := runProbe(ctx, dir, 4096, "ps", "-eo", "pid,pcpu,pmem,comm", "-r")
	if err != nil {
		out, err = runProbe(ctx, dir, 4096, "ps", "-eo", "pid,pcpu,pmem,comm")
	}
	if err != nil {
		return ""
	}
	return headLines(out, 10)
}

// systemdServices lists service units.
func systemdServices(ctx context.Context, dir string) string {
	out, err := runProbe(ctx, dir, 8192, "systemctl", "list-units", "--type=service", "--no-pager", "--no-legend")
	if err != nil {
		return ""
	}
	return headLines(out, 30)
}

// ecosystemMarkers maps workspace signature files to ecosystem names.
var ecosystemMarkers = []struct {
	file string
	name string
}{
	{"package.json", "node"},
	{"go.mod", "go"},
	{"Cargo.toml", "rust"},
	{"pyproject.toml", "python"},
	{"requirements.txt", "python"},
	{"justfile", "just"},
	{"Justfile", "just"},
	{"Makefile", "make"},
	{"makefile", "make"},
	{"Dockerfile", "docker"},
	{"Chart.yaml", "helm"},
}

// gatherUniversal is the universal workspace provider: detected ecosystems,
// scripts/tasks, a bounded directory listing, git state, and allowlisted
// command help. Total budget 1.2s (PERF-005).
func gatherUniversal(ctx context.Context, req Request, tokens []string) Context {
	ctx, cancel := context.WithTimeout(ctx, universalBudget)
	defer cancel()

	c := Context{
		Ecosystems: detectEcosystems(req.CWD),
		Scripts:    gatherScripts(req.CWD),
		DirEntries: listDirEntries(req.CWD, maxDirEntries),
	}

	// Independent probes run concurrently; each writes its own Context field
	// and the WaitGroup establishes happens-before for the reads below.
	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		gatherGit(ctx, req.CWD, &c)
	}()
	go func() {
		defer wg.Done()
		c.Help = gatherHelp(ctx, req.CWD, tokens)
	}()
	wg.Wait()
	return c
}

// detectEcosystems reports the workspace signatures present in dir, in a
// stable order without duplicates.
func detectEcosystems(dir string) []string {
	seen := make(map[string]bool, len(ecosystemMarkers))
	var out []string
	for _, m := range ecosystemMarkers {
		if seen[m.name] {
			continue
		}
		if st, err := os.Stat(filepath.Join(dir, m.file)); err == nil && !st.IsDir() {
			seen[m.name] = true
			out = append(out, m.name)
		}
	}
	return out
}

// makeTargetRe matches simple Make targets ("build: deps"), capturing the
// text after the colon so assignments ("X := 1") can be excluded.
var makeTargetRe = regexp.MustCompile(`^([A-Za-z0-9_][A-Za-z0-9_.%-]*)\s*:(.*)$`)

// justTargetRe matches just recipes ("build:" or "build target: deps"),
// capturing the text after the colon so assignments can be excluded.
var justTargetRe = regexp.MustCompile(`^([A-Za-z_][A-Za-z0-9_-]*)(?:\s[^:=]*)?:(.*)$`)

// gatherScripts extracts package.json scripts and Make/Just targets from dir
// and up to 15 visible immediate subdirectories, excluding node_modules.
// Entries from subdirectories are prefixed with the directory name.
func gatherScripts(dir string) []string {
	var scripts []string
	collectDirScripts(dir, "", &scripts)

	entries, err := os.ReadDir(dir)
	if err == nil {
		n := 0
		for _, e := range entries {
			if n >= 15 {
				break
			}
			if !e.IsDir() || strings.HasPrefix(e.Name(), ".") || e.Name() == "node_modules" {
				continue
			}
			n++
			collectDirScripts(filepath.Join(dir, e.Name()), e.Name(), &scripts)
		}
	}
	if len(scripts) > maxScripts {
		scripts = scripts[:maxScripts]
	}
	return scripts
}

// collectDirScripts appends the scripts/targets of a single directory.
// prefix qualifies names from subdirectories ("web/build").
func collectDirScripts(dir, prefix string, out *[]string) {
	qualify := func(name string) string {
		if prefix == "" {
			return name
		}
		return prefix + "/" + name
	}

	if data, err := readBounded(filepath.Join(dir, "package.json"), maxPackageJSONBytes); err == nil {
		var pkg struct {
			Scripts map[string]string `json:"scripts"`
		}
		if json.Unmarshal(data, &pkg) == nil && len(pkg.Scripts) > 0 {
			names := make([]string, 0, len(pkg.Scripts))
			for name := range pkg.Scripts {
				names = append(names, name)
			}
			sort.Strings(names)
			for _, name := range names {
				*out = append(*out, qualify(name)+": "+pkg.Scripts[name])
			}
		}
	}

	for _, names := range []struct {
		files []string
		re    *regexp.Regexp
	}{
		{[]string{"Makefile", "makefile"}, makeTargetRe},
		{[]string{"justfile", "Justfile"}, justTargetRe},
	} {
		for _, file := range names.files {
			data, err := readBounded(filepath.Join(dir, file), maxBuildFileBytes)
			if err != nil {
				continue
			}
			seen := map[string]bool{}
			for _, line := range strings.Split(string(data), "\n") {
				m := names.re.FindStringSubmatch(line)
				if m == nil {
					continue
				}
				target := m[1]
				if strings.HasPrefix(m[2], "=") {
					continue // assignment, not a target
				}
				if strings.HasPrefix(target, ".") || strings.Contains(target, "%") || seen[target] {
					continue
				}
				seen[target] = true
				*out = append(*out, qualify(target))
			}
			break // first matching build file wins
		}
	}
}

// listDirEntries returns up to max visible entry names of dir; directories
// carry a trailing slash. Hidden entries are excluded: only visible file and
// directory names may be disclosed (PRD 11.2).
func listDirEntries(dir string, max int) []string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	out := make([]string, 0, max)
	for _, e := range entries {
		if len(out) >= max {
			break
		}
		if strings.HasPrefix(e.Name(), ".") {
			continue
		}
		name := e.Name()
		if e.IsDir() {
			name += "/"
		}
		out = append(out, name)
	}
	return out
}

// gatherGit fills the git fields of c from independent concurrent probes,
// each with a sub-second budget. Outside a git workspace every probe fails
// and the fields stay empty.
func gatherGit(ctx context.Context, dir string, c *Context) {
	var wg sync.WaitGroup
	probe := func(dst *string, maxOut int, args ...string) {
		wg.Add(1)
		go func() {
			defer wg.Done()
			*dst = gitProbe(ctx, dir, maxOut, args...)
		}()
	}

	var branches, commits, gitDir string
	probe(&c.GitBranch, 200, "rev-parse", "--abbrev-ref", "HEAD")
	probe(&c.GitPrevBranch, 200, "rev-parse", "--abbrev-ref", "@{-1}")
	probe(&c.GitStatus, maxStatusChars, "status", "--short")
	probe(&c.StagedDiff, maxStagedChars, "diff", "--staged")
	probe(&branches, maxSpecialChars, "for-each-ref", "--sort=-committerdate", "--count=10", "--format=%(refname:short)", "refs/heads/")
	probe(&commits, maxSpecialChars, "log", "-5", "--format=%s")
	probe(&gitDir, 1000, "rev-parse", "--absolute-git-dir")
	wg.Wait()

	if c.GitBranch == "" {
		// Unborn HEAD (fresh repository without commits).
		c.GitBranch = gitProbe(ctx, dir, 200, "symbolic-ref", "--short", "HEAD")
	}
	if branches != "" {
		c.GitBranches = splitLines(branches, maxGitBranches)
	}
	if commits != "" {
		c.RecentCommits = splitLines(commits, maxRecentCommits)
	}
	if gitDir == "" {
		gitDir = resolveGitDir(dir)
	}
	c.MergeState = mergeState(gitDir)
}

// gitProbe runs one git probe with a sub-second budget; failure yields "".
func gitProbe(ctx context.Context, dir string, maxOut int, args ...string) string {
	ctx, cancel := context.WithTimeout(ctx, gitProbeBudget)
	defer cancel()
	out, err := runProbe(ctx, dir, maxOut, "git", args...)
	if err != nil {
		return ""
	}
	return out
}

// resolveGitDir returns the git directory for cwd when it is directly
// present, following a "gitdir:" file for worktrees and submodules.
func resolveGitDir(cwd string) string {
	dot := filepath.Join(cwd, ".git")
	st, err := os.Stat(dot)
	if err != nil {
		return ""
	}
	if st.IsDir() {
		return dot
	}
	data, err := readBounded(dot, 4096)
	if err != nil {
		return ""
	}
	p, ok := strings.CutPrefix(strings.TrimSpace(string(data)), "gitdir:")
	if !ok {
		return ""
	}
	p = strings.TrimSpace(p)
	if p == "" {
		return ""
	}
	if !filepath.IsAbs(p) {
		p = filepath.Join(cwd, p)
	}
	return p
}

// mergeState reports "merging", "rebasing", or "" for a git directory.
func mergeState(gitDir string) string {
	if gitDir == "" {
		return ""
	}
	if isFile(filepath.Join(gitDir, "MERGE_HEAD")) {
		return "merging"
	}
	if isDir(filepath.Join(gitDir, "rebase-merge")) || isDir(filepath.Join(gitDir, "rebase-apply")) {
		return "rebasing"
	}
	return ""
}

// helpAllowlist is the 1.0 set of commands for which bounded --help output
// may be gathered.
var helpAllowlist = map[string]bool{
	"git": true, "docker": true, "kubectl": true, "npm": true, "yarn": true,
	"pnpm": true, "cargo": true, "go": true, "systemctl": true, "helm": true,
	"terraform": true, "aws": true, "gcloud": true, "az": true, "make": true,
	"bun": true, "pip": true, "python": true, "python3": true, "node": true,
	"deno": true, "tar": true, "curl": true, "wget": true, "ssh": true,
	"podman": true, "tofu": true, "ansible": true, "gh": true, "nix": true,
}

// helpAllowed reports whether --help may be gathered for root: it must be on
// the explicit allowlist and contain no path separators (PRD 11.3).
func helpAllowed(root string) bool {
	if root == "" || strings.ContainsAny(root, `/\`) {
		return false
	}
	return helpAllowlist[root]
}

// gatherHelp runs "<root> --help" capped at 600 chars within 900ms, only when
// the buffer is a bare allowlisted root or root+partial-subcommand
// (len(tokens)<=2 keeps it cheap).
func gatherHelp(ctx context.Context, dir string, tokens []string) string {
	if len(tokens) == 0 || len(tokens) > 2 || !helpAllowed(tokens[0]) {
		return ""
	}
	ctx, cancel := context.WithTimeout(ctx, helpBudget)
	defer cancel()
	out, err := runProbe(ctx, dir, maxHelpChars, tokens[0], "--help")
	if err != nil {
		return ""
	}
	return out
}

// boundedWriter discards everything past n bytes while reporting full writes
// so the producing process never blocks or dies on a full buffer.
type boundedWriter struct {
	w io.Writer
	n int64
}

func (b *boundedWriter) Write(p []byte) (int, error) {
	if b.n > 0 {
		chunk := p
		if int64(len(chunk)) > b.n {
			chunk = chunk[:b.n]
		}
		if _, err := b.w.Write(chunk); err != nil {
			return 0, err
		}
		b.n -= int64(len(chunk))
	}
	return len(p), nil
}

// runProbe executes one external probe: argument array (never a shell
// string), working directory set to dir, ctx deadline, stdout bounded at
// maxOut bytes (PRD 11.3). Stderr is discarded. Errors are returned so
// callers can fall back; gathering code turns them into empty fields.
func runProbe(ctx context.Context, dir string, maxOut int, name string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	if dir != "" {
		cmd.Dir = dir
	}
	var buf bytes.Buffer
	cmd.Stdout = &boundedWriter{w: &buf, n: int64(maxOut + 1)}
	if err := cmd.Run(); err != nil {
		return "", err
	}
	return capString(strings.TrimSpace(buf.String()), maxOut), nil
}

// readBounded reads a file capped at max bytes.
func readBounded(path string, max int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	return io.ReadAll(io.LimitReader(f, max))
}

// capString hard-caps s at max bytes without splitting a UTF-8 rune at the
// cut point.
func capString(s string, max int) string {
	if len(s) <= max {
		return s
	}
	if max < 0 {
		max = 0
	}
	return strings.ToValidUTF8(s[:max], "")
}

// headLines keeps the first max lines of s.
func headLines(s string, max int) string {
	if max <= 0 {
		return ""
	}
	lines := strings.Split(s, "\n")
	if len(lines) <= max {
		return s
	}
	return strings.Join(lines[:max], "\n")
}

// splitLines splits probe output into at most max lines.
func splitLines(s string, max int) []string {
	lines := strings.Split(s, "\n")
	if len(lines) > max {
		lines = lines[:max]
	}
	return lines
}

func isFile(path string) bool {
	st, err := os.Stat(path)
	return err == nil && !st.IsDir()
}

func isDir(path string) bool {
	st, err := os.Stat(path)
	return err == nil && st.IsDir()
}
