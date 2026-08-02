package generators

import (
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/complete"
)

const gitTimeout = 5 * time.Second

// GitBranches suggests local and remote branches according to the Git
// policy in ctx. Branch-creation flags suppress existing-branch results at
// the spec level by pointing the flag at a different node.
func GitBranches() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		return gitBranchCandidates(ctx, prefix, true)
	}
}

// GitCheckoutTargets combines branches and files for git checkout/switch.
func GitCheckoutTargets() complete.Generator {
	filesGen := Files()
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		out := gitBranchCandidates(ctx, prefix, true)
		for _, c := range filesGen(ctx, args, prefix) {
			c.Priority = 20
			out = append(out, c)
		}
		return out
	}
}

// GitResetTargets combines branches, commits, and files for git reset.
func GitResetTargets() complete.Generator {
	filesGen := Files()
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		out := gitBranchCandidates(ctx, prefix, false)
		out = append(out, gitCommits(ctx, prefix)...)
		out = append(out, filesGen(ctx, args, prefix)...)
		return out
	}
}

// GitShowTargets combines tags and commits for git show.
func GitShowTargets() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		out := gitTags(ctx, prefix)
		return append(out, gitCommits(ctx, prefix)...)
	}
}

// GitRefs suggests branches, tags, stashes, and recent commits.
func GitRefs() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		out := gitBranchCandidates(ctx, prefix, false)
		out = append(out, gitTags(ctx, prefix)...)
		out = append(out, gitStashes(ctx, prefix)...)
		out = append(out, gitCommits(ctx, prefix)...)
		return out
	}
}

// GitPushPull suggests remotes first, then local branches once a remote is
// present, and stops after both positional values exist.
func GitPushPull() complete.Generator {
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		switch len(args) {
		case 0:
			var out []complete.Candidate
			for _, r := range lines(run(ctx.CWD, gitTimeout, "git", "remote")) {
				if !hasFoldPrefix(r, prefix) {
					continue
				}
				out = append(out, complete.Candidate{
					Title: r, Description: "remote", Icon: "git", Priority: 60,
				})
			}
			return out
		case 1:
			return gitBranchCandidates(ctx, prefix, false)
		default:
			return nil
		}
	}
}

func gitBranchCandidates(ctx complete.Context, prefix string, filterActive bool) []complete.Candidate {
	current := strings.TrimSpace(run(ctx.CWD, gitTimeout, "git", "rev-parse", "--abbrev-ref", "HEAD"))
	var out []complete.Candidate
	seen := map[string]bool{}
	addBranch := func(name, desc string, priority int) {
		if name == "" || !hasFoldPrefix(name, prefix) {
			return
		}
		if filterActive && ctx.GitFilterActiveBranch && name == current {
			return
		}
		if seen[name] {
			return
		}
		seen[name] = true
		out = append(out, complete.Candidate{
			Title: name, Description: desc, Icon: "git", Priority: priority,
		})
	}
	for _, b := range lines(run(ctx.CWD, gitTimeout, "git", "for-each-ref", "--format=%(refname:short)", "refs/heads")) {
		addBranch(b, "local branch", 60)
	}
	for _, b := range lines(run(ctx.CWD, gitTimeout, "git", "for-each-ref", "--format=%(refname:short)", "refs/remotes")) {
		if strings.HasSuffix(b, "/HEAD") {
			continue
		}
		if ctx.GitDeduplicateBranches {
			if _, short, ok := strings.Cut(b, "/"); ok && seen[short] {
				continue
			}
		}
		addBranch(b, "remote branch", 40)
	}
	return out
}

func gitTags(ctx complete.Context, prefix string) []complete.Candidate {
	var out []complete.Candidate
	for _, t := range lines(run(ctx.CWD, gitTimeout, "git", "tag", "--sort=-creatordate")) {
		if !hasFoldPrefix(t, prefix) {
			continue
		}
		out = append(out, complete.Candidate{Title: t, Description: "tag", Icon: "git", Priority: 40})
	}
	return out
}

func gitStashes(ctx complete.Context, prefix string) []complete.Candidate {
	var out []complete.Candidate
	for _, ln := range lines(run(ctx.CWD, gitTimeout, "git", "stash", "list", "--format=%gd\t%gs")) {
		name, subject, _ := strings.Cut(ln, "\t")
		if !hasFoldPrefix(name, prefix) {
			continue
		}
		out = append(out, complete.Candidate{Title: name, Description: subject, Icon: "git", Priority: 35})
	}
	return out
}

// gitCommits returns the 30 most recent commit hashes with age and message.
func gitCommits(ctx complete.Context, prefix string) []complete.Candidate {
	var out []complete.Candidate
	for _, ln := range lines(run(ctx.CWD, gitTimeout, "git", "log", "-30", "--format=%h\t%cr\t%s")) {
		hash, rest, _ := strings.Cut(ln, "\t")
		if !hasFoldPrefix(hash, prefix) {
			continue
		}
		age, subject, _ := strings.Cut(rest, "\t")
		out = append(out, complete.Candidate{
			Title: hash, Description: age + " — " + subject, Icon: "git", Priority: 25,
		})
	}
	return out
}
