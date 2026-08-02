package sources

import (
	"context"
	"regexp"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/core"
)

// gitProbeTimeout bounds git probes (PRD 11.3).
const gitProbeTimeout = 2 * time.Second

// branchCreateFlags suppress existing-branch suggestions.
var branchCreateFlags = map[string]bool{
	"-b": true, "-B": true, "-c": true, "-C": true, "--orphan": true,
}

func hasBranchCreateFlag(args []string) bool {
	for _, a := range args {
		if branchCreateFlags[a] || strings.HasPrefix(a, "--orphan=") {
			return true
		}
	}
	return false
}

// gitRefs lists local and remote branch short names plus the active branch.
func (s *Sources) gitRefs(ctx context.Context, cwd string) (locals []string, active string, remotes []string, err error) {
	out, err := probe(ctx, gitProbeTimeout, cwd, "git", "for-each-ref",
		"--format=%(refname)%09%(refname:short)%09%(HEAD)", "refs/heads", "refs/remotes")
	if err != nil {
		return nil, "", nil, err
	}
	for _, line := range splitLines(out) {
		fields := strings.Split(line, "\t")
		if len(fields) < 3 {
			continue
		}
		full, short, head := fields[0], fields[1], strings.TrimSpace(fields[2])
		if short == "" {
			continue
		}
		if strings.HasPrefix(full, "refs/remotes/") {
			if strings.HasSuffix(short, "/HEAD") {
				continue
			}
			remotes = append(remotes, short)
			continue
		}
		if head == "*" {
			active = short
		}
		locals = append(locals, short)
	}
	return locals, active, remotes, nil
}

// branchSuggestions lists branches honoring git.filter-active-branch and
// git.deduplicate-branches (PRD 9.8 "Git branch policy").
func (s *Sources) branchSuggestions(ctx context.Context, cwd string) []core.Suggestion {
	locals, active, remotes, err := s.gitRefs(ctx, cwd)
	if err != nil {
		return nil
	}
	cfg := s.config()
	filterActive := cfg.Git.FilterActiveBranch
	dedup := cfg.Git.DeduplicateBranches

	var res []core.Suggestion
	localSet := make(map[string]bool, len(locals))
	for _, name := range locals {
		if filterActive && name == active {
			continue
		}
		localSet[name] = true
		res = append(res, dyn(name, "branch", "git"))
	}
	for _, name := range remotes {
		base := name
		if i := strings.IndexByte(name, '/'); i >= 0 {
			base = name[i+1:]
		}
		if dedup {
			// A local branch with the same name shadows the remote one.
			if localSet[base] || (filterActive && base == active) {
				continue
			}
		}
		res = append(res, dyn(name, "remote branch", "git"))
	}
	return res
}

// localBranchSuggestions lists local branches without branch policy.
func (s *Sources) localBranchSuggestions(ctx context.Context, cwd string) []core.Suggestion {
	lines, err := probeLines(ctx, gitProbeTimeout, cwd, "git",
		"for-each-ref", "--format=%(refname:short)", "refs/heads")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, name := range lines {
		name = strings.TrimSpace(name)
		if name != "" {
			res = append(res, dyn(name, "branch", "git"))
		}
	}
	return res
}

func (s *Sources) gitBranchesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	if hasBranchCreateFlag(req.Args) {
		return nil
	}
	return s.branchSuggestions(ctx, req.CWD)
}

func (s *Sources) gitRemotesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	lines, err := probeLines(ctx, gitProbeTimeout, req.CWD, "git", "remote")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, name := range lines {
		name = strings.TrimSpace(name)
		if name != "" {
			res = append(res, dyn(name, "remote", "git"))
		}
	}
	return res
}

func (s *Sources) gitTagsGen(ctx context.Context, req GenRequest) []core.Suggestion {
	lines, err := probeLines(ctx, gitProbeTimeout, req.CWD, "git", "tag", "--list")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, name := range lines {
		name = strings.TrimSpace(name)
		if name != "" {
			res = append(res, dyn(name, "tag", "git"))
		}
	}
	return res
}

// gitCommitSuggestions lists the 30 most recent commits with
// "age · message" descriptions.
func (s *Sources) gitCommitSuggestions(ctx context.Context, cwd string) []core.Suggestion {
	lines, err := probeLines(ctx, 3*time.Second, cwd, "git",
		"log", "-30", "--format=%h%x09%cr%x09%s")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) < 3 || parts[0] == "" {
			continue
		}
		res = append(res, dyn(parts[0], parts[1]+" · "+parts[2], "git"))
	}
	return res
}

func (s *Sources) gitCommitsGen(ctx context.Context, req GenRequest) []core.Suggestion {
	return s.gitCommitSuggestions(ctx, req.CWD)
}

var stashLineRe = regexp.MustCompile(`^(stash@\{\d+\}):\s*(.*)$`)

func (s *Sources) gitStashesGen(ctx context.Context, req GenRequest) []core.Suggestion {
	lines, err := probeLines(ctx, gitProbeTimeout, req.CWD, "git", "stash", "list")
	if err != nil {
		return nil
	}
	var res []core.Suggestion
	for _, line := range lines {
		m := stashLineRe.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		res = append(res, dyn(m[1], m[2], "git"))
	}
	return res
}

// gitPushPullGen suggests remotes first, then local branches once a remote
// is present, and nothing after both positionals (PRD 9.8 "Git push/pull").
func (s *Sources) gitPushPullGen(ctx context.Context, req GenRequest) []core.Suggestion {
	switch len(req.Args) {
	case 0:
		return s.gitRemotesGen(ctx, req)
	case 1:
		return s.localBranchSuggestions(ctx, req.CWD)
	default:
		return nil
	}
}

// gitMixedGen combines branches (per policy) and files, branches first
// (checkout/reset).
func (s *Sources) gitMixedGen(ctx context.Context, req GenRequest) []core.Suggestion {
	var res []core.Suggestion
	if !hasBranchCreateFlag(req.Args) {
		res = s.branchSuggestions(ctx, req.CWD)
	}
	files := s.CompleteFiles(FileRequest{
		Partial:    req.Partial,
		CWD:        req.CWD,
		Mode:       FileAny,
		ShowHidden: s.config().UI.HiddenFiles,
	})
	return append(res, files...)
}

// gitShowGen combines tags and commits.
func (s *Sources) gitShowGen(ctx context.Context, req GenRequest) []core.Suggestion {
	res := s.gitTagsGen(ctx, req)
	return append(res, s.gitCommitSuggestions(ctx, req.CWD)...)
}
