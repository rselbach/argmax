package sources

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/core"
)

// genProbeTimeout is the default dynamic generator deadline (PERF-006).
const genProbeTimeout = 5 * time.Second

// pkgProbeTimeout bounds installed-system-package enumeration (PERF-006).
const pkgProbeTimeout = 8 * time.Second

// ---------- Docker / Podman ----------

// dockerContainerSuggestions lists containers with image descriptions.
func (s *Sources) dockerContainerSuggestions(ctx context.Context, root string, all bool) []core.Suggestion {
	args := []string{"ps"}
	if all {
		args = append(args, "-a")
	}
	args = append(args, "--format", "{{.Names}}\t{{.Image}}")
	lines, err := probeLines(ctx, genProbeTimeout, "", root, args...)
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		name, image, _ := strings.Cut(line, "\t")
		name = strings.TrimSpace(name)
		if name == "" {
			continue
		}
		desc := strings.TrimSpace(image)
		if desc == "" {
			desc = "container"
		}
		res = append(res, dyn(name, desc, root))
	}
	return res
}

func (s *Sources) dockerContainersGen(ctx context.Context, req GenRequest, all bool) []core.Suggestion {
	return s.dockerContainerSuggestions(ctx, req.RootCmd, all)
}

// dockerImageSuggestions lists local images, omitting dangling names and
// duplicate tags.
func (s *Sources) dockerImageSuggestions(ctx context.Context, root string) []core.Suggestion {
	lines, err := probeLines(ctx, genProbeTimeout, "", root,
		"images", "--format", "{{.Repository}}:{{.Tag}}")
	if err != nil {
		return nil
	}
	seen := make(map[string]bool)
	var res []core.Suggestion
	for _, line := range lines {
		name := strings.TrimSpace(line)
		if name == "" || strings.Contains(name, "<none>") || seen[name] {
			continue
		}
		seen[name] = true
		res = append(res, dyn(name, "image", root))
	}
	return res
}

func (s *Sources) dockerImagesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	return s.dockerImageSuggestions(ctx, req.RootCmd)
}

// dockerInspectGen combines all containers and images.
func (s *Sources) dockerInspectGen(ctx context.Context, req GenRequest) []core.Suggestion {
	res := s.dockerContainerSuggestions(ctx, req.RootCmd, true)
	return append(res, s.dockerImageSuggestions(ctx, req.RootCmd)...)
}

// ---------- SSH hosts ----------

// sshHostsGen parses user and system SSH config Host lines, skipping
// wildcard/negated patterns and de-duplicating (PRD 9.8 "SSH").
func (s *Sources) sshHostsGen(req GenRequest) []core.Suggestion {
	home, _ := os.UserHomeDir()
	files := []string{
		filepath.Join(home, ".ssh", "config"),
		"/etc/ssh/ssh_config",
	}
	seen := make(map[string]bool)
	var res []core.Suggestion
	for _, f := range files {
		data, err := os.ReadFile(f)
		if err != nil {
			continue
		}
		for _, line := range strings.Split(string(data), "\n") {
			fields := strings.Fields(line)
			if len(fields) < 2 || !strings.EqualFold(fields[0], "host") {
				continue
			}
			for _, h := range fields[1:] {
				if h == "" || strings.ContainsAny(h, "*?!") || seen[h] {
					continue
				}
				seen[h] = true
				res = append(res, dyn(h, "ssh host", "ssh"))
			}
		}
	}
	sortSuggestions(res)
	return res
}

// ---------- Node package scripts ----------

// nodePriorityScripts get a higher confidence (PRD 9.8 "Node package scripts").
var nodePriorityScripts = map[string]bool{
	"dev": true, "start": true, "build": true, "test": true,
	"lint": true, "preview": true, "typecheck": true, "format": true,
}

// nodeScriptsGen reads package.json scripts from the nearest manifest
// walking up from CWD, or returns common placeholders when no manifest
// exists anywhere up the tree.
func (s *Sources) nodeScriptsGen(req GenRequest) []core.Suggestion {
	manifest := findUpwards(req.CWD, "package.json")
	if manifest == "" {
		var res []core.Suggestion
		for _, name := range []string{"dev", "start", "build", "test", "lint"} {
			res = append(res, dyn(name, "common script", "node"))
		}
		return res
	}
	data, err := os.ReadFile(manifest)
	if err != nil {
		return nil
	}
	var pkg struct {
		Scripts map[string]string `json:"scripts"`
	}
	if err := json.Unmarshal(data, &pkg); err != nil || len(pkg.Scripts) == 0 {
		return nil
	}
	names := make([]string, 0, len(pkg.Scripts))
	for name := range pkg.Scripts {
		names = append(names, name)
	}
	sort.Strings(names)
	var res []core.Suggestion
	for _, name := range names {
		conf := 70
		if nodePriorityScripts[name] {
			conf = 85
		}
		res = append(res, dynConf(name, pkg.Scripts[name], "node", conf))
	}
	return res
}

// findUpwards locates name in dir or its nearest ancestor.
func findUpwards(dir, name string) string {
	if dir == "" {
		return ""
	}
	dir = filepath.Clean(dir)
	for {
		cand := filepath.Join(dir, name)
		if fi, err := os.Stat(cand); err == nil && !fi.IsDir() {
			return cand
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return ""
		}
		dir = parent
	}
}

// ---------- Just recipes ----------

var justRecipeNameRe = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_-]*$`)

// justRecipesGen parses justfile/Justfile recipes in CWD. The comment
// immediately above a recipe becomes its description.
func (s *Sources) justRecipesGen(req GenRequest) []core.Suggestion {
	var data []byte
	for _, name := range []string{"justfile", "Justfile"} {
		if b, err := os.ReadFile(filepath.Join(req.CWD, name)); err == nil {
			data = b
			break
		}
	}
	if data == nil {
		return nil
	}
	seen := make(map[string]bool)
	var res []core.Suggestion
	comment := ""
	for _, raw := range strings.Split(string(data), "\n") {
		if raw == "" || strings.HasPrefix(raw, "#") {
			if strings.HasPrefix(raw, "#") {
				comment = strings.TrimSpace(strings.TrimPrefix(raw, "#"))
			} else {
				comment = ""
			}
			continue
		}
		if raw[0] == ' ' || raw[0] == '\t' {
			comment = ""
			continue
		}
		line := strings.TrimSpace(raw)
		if strings.HasPrefix(line, "set ") || strings.HasPrefix(line, "alias ") ||
			strings.HasPrefix(line, "import ") || strings.HasPrefix(line, "mod ") {
			comment = ""
			continue
		}
		idx := strings.IndexByte(line, ':')
		if idx <= 0 || (idx+1 < len(line) && line[idx+1] == '=') {
			comment = ""
			continue
		}
		fields := strings.Fields(line[:idx])
		if len(fields) == 0 || !justRecipeNameRe.MatchString(fields[0]) {
			comment = ""
			continue
		}
		name := fields[0]
		desc := comment
		comment = ""
		if desc == "" {
			desc = "recipe"
		}
		if seen[name] {
			continue
		}
		seen[name] = true
		res = append(res, dyn(name, desc, genIcon(req.ID, req.RootCmd)))
	}
	return res
}

// ---------- Make targets ----------

var makeTargetRe = regexp.MustCompile(`^([A-Za-z0-9_.%/-]+)\s*:`)

// makeTargetsGen parses visible Makefile targets in CWD, excluding
// dot-prefixed pseudo-targets and pattern/variable lines.
func (s *Sources) makeTargetsGen(req GenRequest) []core.Suggestion {
	var data []byte
	for _, name := range []string{"Makefile", "makefile", "GNUmakefile"} {
		if b, err := os.ReadFile(filepath.Join(req.CWD, name)); err == nil {
			data = b
			break
		}
	}
	if data == nil {
		return nil
	}
	seen := make(map[string]bool)
	var res []core.Suggestion
	for _, raw := range strings.Split(string(data), "\n") {
		if raw == "" || raw[0] == ' ' || raw[0] == '\t' || raw[0] == '#' {
			continue
		}
		if strings.Contains(raw, "=") {
			continue
		}
		m := makeTargetRe.FindStringSubmatch(raw)
		if m == nil {
			continue
		}
		name := m[1]
		if strings.HasPrefix(name, ".") || strings.Contains(name, "%") || seen[name] {
			continue
		}
		seen[name] = true
		res = append(res, dyn(name, "make target", genIcon(req.ID, req.RootCmd)))
	}
	return res
}

// ---------- Zoxide ----------

// zoxideDirsGen merges local directory completion with `zoxide query -l`
// results (PRD 9.8 "Zoxide").
func (s *Sources) zoxideDirsGen(ctx context.Context, req GenRequest) []core.Suggestion {
	res := s.CompleteFiles(FileRequest{
		Partial:    req.Partial,
		CWD:        req.CWD,
		Mode:       FileDir,
		ShowHidden: s.config().UI.HiddenFiles,
	})
	lines, err := probeLines(ctx, 2*time.Second, "", "zoxide", "query", "-l")
	if err != nil {
		return res
	}
	limit := 10
	if req.Partial == "" {
		limit = 20
	}
	count := 0
	for _, line := range lines {
		if count >= limit {
			break
		}
		p := strings.TrimSpace(line)
		if p == "" {
			continue
		}
		// Match against the whole partial so multi-word directories work.
		if req.Partial != "" && !isSubsequence(req.Partial, p) {
			continue
		}
		res = append(res, dyn(p, "directory", genIcon(req.ID, req.RootCmd)))
		count++
	}
	return res
}

// isSubsequence reports whether needle is a case-insensitive subsequence of
// haystack.
func isSubsequence(needle, haystack string) bool {
	n := strings.ToLower(needle)
	h := strings.ToLower(haystack)
	i := 0
	for j := 0; j < len(h) && i < len(n); j++ {
		if h[j] == n[i] {
			i++
		}
	}
	return i == len(n)
}

// ---------- Installed packages ----------

// packagesInstalledGen enumerates installed system packages per package
// manager (PRD 9.8 "Installed system packages"). Missing tools yield nil.
func (s *Sources) packagesInstalledGen(ctx context.Context, req GenRequest) []core.Suggestion {
	switch req.RootCmd {
	case "pacman", "yay", "paru":
		return s.simplePkgProbe(ctx, "pacman", "package", "-Qq")
	case "apt", "apt-get":
		if _, err := exec.LookPath("dpkg-query"); err != nil {
			return nil
		}
		lines, err := probeLines(ctx, pkgProbeTimeout, "", "dpkg-query",
			"-W", `-f=${Package}\t${Version}\n`)
		if err != nil {
			return nil
		}
		var res []core.Suggestion
		for _, line := range lines {
			name, version, _ := strings.Cut(line, "\t")
			name = strings.TrimSpace(name)
			if name == "" {
				continue
			}
			desc := strings.TrimSpace(version)
			if desc == "" {
				desc = "package"
			}
			res = append(res, dyn(name, desc, "misc"))
		}
		return res
	case "dnf", "yum":
		return s.simplePkgProbe(ctx, "rpm", "package", "-qa")
	case "brew":
		if _, err := exec.LookPath("brew"); err != nil {
			return nil
		}
		var res []core.Suggestion
		if lines, err := probeLines(ctx, pkgProbeTimeout, "", "brew", "leaves"); err == nil {
			for _, name := range lines {
				if name = strings.TrimSpace(name); name != "" {
					res = append(res, dyn(name, "formula", "misc"))
				}
			}
		}
		if lines, err := probeLines(ctx, pkgProbeTimeout, "", "brew", "list", "--cask"); err == nil {
			for _, name := range lines {
				if name = strings.TrimSpace(name); name != "" {
					res = append(res, dyn(name, "cask", "misc"))
				}
			}
		}
		return res
	}
	return nil
}

// simplePkgProbe runs a package listing probe whose output is plain names.
func (s *Sources) simplePkgProbe(ctx context.Context, tool, desc string, args ...string) []core.Suggestion {
	if _, err := exec.LookPath(tool); err != nil {
		return nil
	}
	lines, err := probeLines(ctx, pkgProbeTimeout, "", tool, args...)
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		if name := strings.TrimSpace(line); name != "" {
			res = append(res, dyn(name, desc, "misc"))
		}
	}
	return res
}

// pipPackagesGen enumerates installed pip packages (pip, falling back to
// pip3) with version descriptions.
func (s *Sources) pipPackagesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	tool := "pip"
	if _, err := exec.LookPath(tool); err != nil {
		tool = "pip3"
		if _, err := exec.LookPath(tool); err != nil {
			return nil
		}
	}
	lines, err := probeLines(ctx, genProbeTimeout, "", tool, "list", "--format=freeze")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		name, version, _ := strings.Cut(strings.TrimSpace(line), "==")
		if name == "" {
			continue
		}
		desc := version
		if desc == "" {
			desc = "package"
		}
		res = append(res, dyn(name, desc, "misc"))
	}
	return res
}

// ---------- Processes ----------

// processesGen lists "PID command name" pairs filtered by numeric prefix.
func (s *Sources) processesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	lines, err := probeLines(ctx, 2*time.Second, "", "ps", "-eo", "pid=,comm=")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		pid, comm := fields[0], strings.Join(fields[1:], " ")
		if req.Partial != "" && !strings.HasPrefix(pid, req.Partial) {
			continue
		}
		res = append(res, dyn(pid, comm, genIcon(req.ID, req.RootCmd)))
	}
	return res
}

// processNamesGen lists unique command names prefix-filtered by Partial.
func (s *Sources) processNamesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	lines, err := probeLines(ctx, 2*time.Second, "", "ps", "-eo", "comm=")
	if err != nil {
		return nil
	}
	seen := make(map[string]bool)
	lower := strings.ToLower(req.Partial)
	var names []string
	for _, line := range lines {
		name := strings.TrimSpace(line)
		if name == "" || seen[name] {
			continue
		}
		if lower != "" && !strings.HasPrefix(strings.ToLower(name), lower) {
			continue
		}
		seen[name] = true
		names = append(names, name)
	}
	sort.Strings(names)
	var res []core.Suggestion
	for _, name := range names {
		res = append(res, dyn(name, "process", genIcon(req.ID, req.RootCmd)))
	}
	return res
}

// ---------- Environment variables ----------

// secretEnvNameRe matches variable names whose values must be redacted.
var secretEnvNameRe = regexp.MustCompile(`(?i)(KEY|TOKEN|SECRET|PASS|PWD|AUTH|CREDENTIAL)`)

// envVarsGen suggests environment variable names with truncated values;
// credential-like values are redacted (PRD 9.8 "Environment").
func (s *Sources) envVarsGen(req GenRequest) []core.Suggestion {
	lower := strings.ToLower(req.Partial)
	var res []core.Suggestion
	for _, kv := range os.Environ() {
		name, value, _ := strings.Cut(kv, "=")
		if name == "" || (lower != "" && !strings.HasPrefix(strings.ToLower(name), lower)) {
			continue
		}
		var desc string
		if secretEnvNameRe.MatchString(name) {
			desc = "••••••"
		} else {
			desc = truncateRunes(value, 40)
		}
		res = append(res, dyn(name, desc, genIcon(req.ID, req.RootCmd)))
	}
	sortSuggestions(res)
	return res
}

func truncateRunes(v string, max int) string {
	r := []rune(v)
	if len(r) <= max {
		return v
	}
	return string(r[:max]) + "…"
}

// ---------- chmod modes ----------

// chmodMode is a static first-positional chmod suggestion.
type chmodMode struct {
	mode string
	desc string
}

var chmodModes = []chmodMode{
	{"+x", "make executable"},
	{"-x", "remove executable permission"},
	{"u+x", "owner executable"},
	{"a+x", "all executable"},
	{"u+rw", "owner read/write"},
	{"go-w", "remove group/other write"},
	{"a-w", "remove all write"},
	{"u+s", "setuid"},
	{"g+s", "setgid"},
	{"+t", "sticky bit"},
	{"755", "rwxr-xr-x"},
	{"644", "rw-r--r--"},
	{"600", "rw-------"},
	{"700", "rwx------"},
	{"777", "rwxrwxrwx"},
	{"666", "rw-rw-rw-"},
	{"400", "r--------"},
	{"444", "r--r--r--"},
	{"750", "rwxr-x---"},
	{"640", "rw-r-----"},
	{"664", "rw-rw-r--"},
}

// chmodModesGen suggests modes as the first positional, then target files
// with executables/scripts first.
func (s *Sources) chmodModesGen(req GenRequest) []core.Suggestion {
	if len(req.Args) == 0 {
		res := make([]core.Suggestion, 0, len(chmodModes))
		for _, m := range chmodModes {
			res = append(res, dyn(m.mode, m.desc, genIcon(req.ID, req.RootCmd)))
		}
		return res
	}
	files := s.CompleteFiles(FileRequest{
		Partial:    req.Partial,
		CWD:        req.CWD,
		Mode:       FileAny,
		ShowHidden: s.config().UI.HiddenFiles,
	})
	// Prefer executable files first (stable).
	var execs, rest []core.Suggestion
	for _, sug := range files {
		if isExecutableSuggestion(req.CWD, sug) {
			execs = append(execs, sug)
		} else {
			rest = append(rest, sug)
		}
	}
	return append(execs, rest...)
}

// isExecutableSuggestion reports whether a file suggestion resolves to an
// executable regular file.
func isExecutableSuggestion(cwd string, sug core.Suggestion) bool {
	if strings.HasSuffix(sug.Text, "/") {
		return false
	}
	typedDir, base := splitPartial(sug.Text)
	fi, err := os.Stat(filepath.Join(resolveDir(typedDir, cwd), base))
	return err == nil && fi.Mode().IsRegular() && fi.Mode().Perm()&0o111 != 0
}
