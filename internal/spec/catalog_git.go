package spec

// gitToolsCount is the PRD 18.8 category size (Git and GitHub tools).
const gitToolsCount = 8

func catalogGit() []*Spec {
	return []*Spec{
		gitSpec(),
		cmd("ghq", "Manage remote repository clones", "git",
			cmd("get", "Clone a remote repository", "git"),
			cmd("list", "List local repositories", "git"),
			cmd("look", "Look into a repository", "git"),
			cmd("root", "Show the ghq root", "git"),
		),
		{
			Name:        "git-cliff",
			Description: "Changelog generator",
			Icon:        "git",
			Options: []Option{
				optD("Generate a config file", "--init"),
				optD("Bump the version", "--bump"),
				optD("Include unreleased changes", "-u", "--unreleased"),
				optGen("files", "Write output to file", "-o", "--output"),
			},
		},
		{
			Name:        "git-flow",
			Description: "Git branching model extensions",
			Icon:        "git",
			Subcommands: []*Spec{
				cmd("init", "Initialize git-flow", "git"),
				cmd("feature", "Manage feature branches", "git",
					cmd("start", "Start a feature branch", "git"),
					cmd("finish", "Finish a feature branch", "git"),
					cmd("publish", "Publish a feature branch", "git"),
					cmd("track", "Track a remote feature branch", "git"),
					cmd("list", "List feature branches", "git"),
				),
				cmd("release", "Manage release branches", "git",
					cmd("start", "Start a release branch", "git"),
					cmd("finish", "Finish a release branch", "git"),
					cmd("publish", "Publish a release branch", "git"),
					cmd("list", "List release branches", "git"),
				),
				cmd("hotfix", "Manage hotfix branches", "git",
					cmd("start", "Start a hotfix branch", "git"),
					cmd("finish", "Finish a hotfix branch", "git"),
					cmd("list", "List hotfix branches", "git"),
				),
				cmd("support", "Manage support branches", "git"),
			},
		},
		cmd("git-profile", "Switch between git profiles", "git",
			cmd("use", "Activate a profile", "git"),
			cmd("list", "List profiles", "git"),
			cmd("add", "Add a profile", "git"),
			cmd("rm", "Remove a profile", "git"),
		),
		cmd("git-quick-stats", "Git repository statistics", "git"),
		cmd("github", "GitHub command-line helper", "github"),
		{
			Name:        "svn",
			Description: "Apache Subversion client",
			Icon:        "vcs",
			Subcommands: []*Spec{
				{Name: "checkout", Aliases: []string{"co"}, Description: "Check out a working copy", Icon: "vcs"},
				{Name: "update", Aliases: []string{"up"}, Description: "Update the working copy", Icon: "vcs"},
				{Name: "commit", Aliases: []string{"ci"}, Description: "Commit changes", Icon: "vcs"},
				cmd("add", "Add files to version control", "vcs"),
				{Name: "delete", Aliases: []string{"rm"}, Description: "Delete files", Icon: "vcs"},
				{Name: "status", Aliases: []string{"st"}, Description: "Show working copy status", Icon: "vcs"},
				cmd("log", "Show commit history", "vcs"),
				cmd("diff", "Show differences", "vcs"),
				cmd("merge", "Merge changes", "vcs"),
				cmd("revert", "Revert local changes", "vcs"),
				cmd("info", "Show working copy info", "vcs"),
			},
		},
	}
}

// gitSpec is the deep git specification (PRD 9.5/9.8).
func gitSpec() *Spec {
	return &Spec{
		Name:        "git",
		Description: "Distributed version control",
		Icon:        "git",
		Options: []Option{
			optD("Show version", "--version"),
			optD("Show help", "--help"),
			optVal("Run as if git was started in path", "-C"),
			optVal("Set a configuration parameter", "-c"),
			optVal("Set the repository path", "--git-dir"),
			optD("Pipe output into the pager", "-p", "--paginate"),
			optD("Do not pipe output into the pager", "-P", "--no-pager"),
		},
		Subcommands: []*Spec{
			{
				Name: "add", Description: "Stage changes", Icon: "git",
				Options: []Option{
					optD("Stage all tracked and untracked files", "-A", "--all"),
					optD("Interactively choose hunks", "-p", "--patch"),
					optD("Stage modified and deleted tracked files", "-u", "--update"),
					optD("Show what would be staged", "-n", "--dry-run"),
					optD("Allow adding ignored files", "-f", "--force"),
					optD("Be verbose", "-v", "--verbose"),
				},
				Generator: "files",
			},
			{
				Name: "bisect", Description: "Binary search for a bad commit", Icon: "git",
				Subcommands: []*Spec{
					cmd("start", "Start a bisect session", "git"),
					cmd("bad", "Mark the current commit bad", "git"),
					cmd("good", "Mark the current commit good", "git"),
					cmd("new", "Mark the current commit new", "git"),
					cmd("old", "Mark the current commit old", "git"),
					cmd("skip", "Skip the current commit", "git"),
					cmd("reset", "End the bisect session", "git"),
					cmd("log", "Show the bisect log", "git"),
					cmd("replay", "Replay a bisect log", "git"),
					cmd("run", "Bisect using a script", "git"),
				},
			},
			{
				Name: "branch", Aliases: []string{"br"}, Description: "List, create, or delete branches", Icon: "git",
				Options: []Option{
					optD("Delete a branch", "-d", "--delete"),
					optD("Force delete a branch", "-D"),
					optD("Move/rename a branch", "-m", "--move"),
					optD("Force move/rename a branch", "-M"),
					optD("List all branches", "-a", "--all"),
					optD("List remote-tracking branches", "-r", "--remotes"),
					optD("Be verbose", "-v"),
					optD("List branches matching a pattern", "--list"),
				},
				Generator: "git-branches",
				MaxArgs:   1,
			},
			{
				Name: "checkout", Aliases: []string{"co"}, Description: "Switch branches or restore files", Icon: "git",
				Options: []Option{
					optVal("Create and switch to a new branch", "-b"),
					optVal("Create/reset and switch to a new branch", "-B"),
					optD("Detach HEAD at the given commit", "--detach"),
					optD("Force checkout", "-f", "--force"),
					optD("Check out our stage on conflicts", "--ours"),
					optD("Check out their stage on conflicts", "--theirs"),
					optVal("Start a new branch from a commit", "--orphan"),
				},
				Generator: "git-checkout",
			},
			{
				Name: "cherry-pick", Description: "Apply commits on top of HEAD", Icon: "git",
				Options: []Option{
					optD("Append a cherry-pick note", "-x"),
					optD("Edit the commit message", "-e", "--edit"),
					optD("Do not create a commit", "-n", "--no-commit"),
					optD("Continue after resolving conflicts", "--continue"),
					optD("Abort the cherry-pick", "--abort"),
				},
				Generator: "git-branches",
			},
			{
				Name: "clean", Description: "Remove untracked files", Icon: "git",
				Options: []Option{
					optD("Force removal", "-f", "--force"),
					optD("Remove untracked directories", "-d"),
					optD("Show what would be removed", "-n", "--dry-run"),
					optD("Clean interactively", "-i", "--interactive"),
					optD("Also remove ignored files", "-x"),
				},
			},
			{
				Name: "clone", Description: "Clone a repository", Icon: "git",
				Options: []Option{
					optVal("Create a shallow clone of depth", "--depth"),
					optVal("Check out branch instead of HEAD", "-b", "--branch"),
					optD("Create a bare repository", "--bare"),
					optD("Create a mirror clone", "--mirror"),
					optD("Clone submodules recursively", "--recursive"),
				},
			},
			{
				Name: "commit", Aliases: []string{"ci"}, Description: "Record changes to the repository", Icon: "git",
				Options: []Option{
					optVal("Commit with the given message", "-m", "--message"),
					optD("Stage modified and deleted files first", "-a", "--all"),
					optD("Amend the previous commit", "--amend"),
					optD("Bypass pre-commit hooks", "--no-verify"),
					optD("Allow an empty commit", "--allow-empty"),
					optD("Add a Signed-off-by trailer", "-s", "--signoff"),
					optD("Be verbose", "-v", "--verbose"),
				},
			},
			{
				Name: "diff", Description: "Show changes between commits", Icon: "git",
				Options: []Option{
					optD("Diff the staged changes", "--cached", "--staged"),
					optD("Show diffstat", "--stat"),
					optD("Show changed file names only", "--name-only"),
					optD("Ignore whitespace", "-w", "--ignore-all-space"),
				},
				Generator: "git-commits",
			},
			{
				Name: "fetch", Description: "Download objects and refs", Icon: "git",
				Options: []Option{
					optD("Fetch all remotes", "--all"),
					optD("Prune deleted remote-tracking branches", "-p", "--prune"),
					optD("Fetch tags", "--tags"),
					optVal("Limit fetching to a number of commits", "--depth"),
				},
			},
			{
				Name: "grep", Description: "Search tracked files", Icon: "git",
				Options: []Option{
					optD("Ignore case", "-i"),
					optD("Show line numbers", "-n", "--line-number"),
					optD("Show file names only", "-l", "--name-only"),
					optD("Use extended regular expressions", "-E"),
					optVal("Use the given pattern", "-e"),
				},
			},
			{
				Name: "init", Description: "Create an empty repository", Icon: "git",
				Options: []Option{
					optD("Create a bare repository", "--bare"),
					optVal("Set the initial branch name", "-b", "--initial-branch"),
				},
			},
			{
				Name: "log", Description: "Show commit history", Icon: "git",
				Options: []Option{
					optD("Condense each commit to one line", "--oneline"),
					optD("Draw a text graph of the history", "--graph"),
					optD("Show all refs", "--all"),
					optVal("Limit the number of commits", "-n", "--max-count"),
					optD("Show diffstat", "--stat"),
					optVal("Pretty-print using the given format", "--pretty"),
					optVal("Limit to commits after a date", "--since"),
				},
				Generator: "git-commits",
			},
			{
				Name: "merge", Description: "Join histories together", Icon: "git",
				Options: []Option{
					optD("Always create a merge commit", "--no-ff"),
					optD("Squash the merged changes", "--squash"),
					optD("Abort the current merge", "--abort"),
					optD("Continue after resolving conflicts", "--continue"),
				},
				Generator: "git-branches",
			},
			{
				Name: "mv", Description: "Move or rename tracked files", Icon: "git",
				Options: []Option{
					optD("Force the move", "-f", "--force"),
					optD("Skip move on errors", "-k"),
				},
				Generator: "files",
			},
			{
				Name: "pull", Description: "Fetch and integrate remote changes", Icon: "git",
				Options: []Option{
					optD("Rebase instead of merging", "-r", "--rebase"),
					optD("Only allow fast-forward merges", "--ff-only"),
					optD("Merge instead of rebasing", "--no-rebase"),
					optD("Squash the merged changes", "--squash"),
				},
				Generator: "git-pushpull",
			},
			{
				Name: "push", Description: "Update remote refs", Icon: "git",
				Options: []Option{
					optD("Set the upstream reference", "-u", "--set-upstream"),
					optD("Force-push, checking the remote ref is as expected", "--force-with-lease"),
					optD("Force-push", "-f", "--force"),
					optD("Delete a remote branch", "-d", "--delete"),
					optD("Push all tags", "--tags"),
				},
				Generator: "git-pushpull",
			},
			{
				Name: "rebase", Description: "Reapply commits on another base", Icon: "git",
				Options: []Option{
					optD("Edit the todo list interactively", "-i", "--interactive"),
					optD("Continue after resolving conflicts", "--continue"),
					optD("Abort the rebase", "--abort"),
					optD("Skip the current commit", "--skip"),
					optVal("Rebase onto the given commit", "--onto"),
				},
				Generator: "git-branches",
			},
			{
				Name: "remote", Description: "Manage tracked repositories", Icon: "git",
				Options: []Option{
					optD("Show remote URLs verbosely", "-v", "--verbose"),
				},
				Subcommands: []*Spec{
					cmd("add", "Add a remote", "git"),
					{Name: "remove", Aliases: []string{"rm"}, Description: "Remove a remote", Icon: "git"},
					cmd("rename", "Rename a remote", "git"),
					cmd("show", "Show a remote", "git"),
					cmd("prune", "Prune stale remote-tracking branches", "git"),
					cmd("update", "Fetch all remotes", "git"),
				},
			},
			{
				Name: "reset", Description: "Reset HEAD to another state", Icon: "git",
				Options: []Option{
					optD("Reset only the HEAD", "--soft"),
					optD("Reset HEAD and the index", "--mixed"),
					optD("Reset HEAD, index, and working tree", "--hard"),
				},
				Generator: "git-reset",
			},
			{
				Name: "restore", Description: "Restore working tree files", Icon: "git",
				Options: []Option{
					optD("Restore the index", "--staged"),
					optVal("Restore from the given commit", "--source"),
					optD("Restore the working tree", "--worktree"),
				},
				Generator: "files",
			},
			{
				Name: "revert", Description: "Revert commits", Icon: "git",
				Options: []Option{
					optD("Edit the commit message", "-e", "--edit"),
					optD("Do not create a commit", "-n", "--no-commit"),
					optD("Continue after resolving conflicts", "--continue"),
					optD("Abort the revert", "--abort"),
				},
				Generator: "git-commits",
			},
			{
				Name: "rm", Description: "Remove tracked files", Icon: "git",
				Options: []Option{
					optD("Recurse into directories", "-r"),
					optD("Remove only from the index", "--cached"),
					optD("Force removal", "-f", "--force"),
					optD("Show what would be removed", "-n", "--dry-run"),
				},
				Generator: "files",
			},
			{
				Name: "show", Description: "Show objects", Icon: "git",
				Options: []Option{
					optD("Show diffstat", "--stat"),
					optD("Show changed file names only", "--name-only"),
					optD("Condense to one line", "--oneline"),
				},
				Generator: "git-show",
			},
			{
				Name: "stash", Description: "Stash working tree changes", Icon: "git",
				Subcommands: []*Spec{
					{
						Name: "push", Description: "Stash the working tree changes", Icon: "git",
						Options: []Option{
							optVal("Stash with the given message", "-m", "--message"),
							optD("Include untracked files", "-u", "--include-untracked"),
							optD("Keep the index intact", "-k", "--keep-index"),
						},
					},
					cmd("pop", "Apply and drop the latest stash", "git"),
					cmd("apply", "Apply a stash", "git"),
					cmd("list", "List stashes", "git"),
					cmd("drop", "Drop a stash", "git"),
					cmd("show", "Show a stash", "git"),
					cmd("clear", "Drop all stashes", "git"),
				},
				Options: []Option{
					optD("Include untracked files", "-u", "--include-untracked"),
					optD("Keep the index intact", "-k", "--keep-index"),
				},
			},
			{
				Name: "status", Aliases: []string{"st"}, Description: "Show the working tree status", Icon: "git",
				Options: []Option{
					optD("Short output", "-s", "--short"),
					optD("Show branch info", "-b", "--branch"),
					optD("Machine-readable output", "--porcelain"),
				},
			},
			{
				Name: "switch", Aliases: []string{"sw"}, Description: "Switch branches", Icon: "git",
				Options: []Option{
					optVal("Create and switch to a new branch", "-c", "--create"),
					optVal("Create/reset and switch to a new branch", "-C", "--force-create"),
					optD("Detach HEAD at the given commit", "--detach"),
					optVal("Switch to a new orphan branch", "--orphan"),
				},
				Generator: "git-branches",
			},
			{
				Name: "tag", Description: "Create, list, and delete tags", Icon: "git",
				Options: []Option{
					optD("Create an annotated tag", "-a", "--annotate"),
					optVal("Tag with the given message", "-m", "--message"),
					optGen("git-tags", "Delete the given tag", "-d", "--delete"),
					optD("List tags", "-l", "--list"),
					optD("Force the tag operation", "-f", "--force"),
				},
			},
			{
				Name: "worktree", Description: "Manage linked working trees", Icon: "git",
				Subcommands: []*Spec{
					cmd("add", "Add a working tree", "git"),
					cmd("list", "List working trees", "git"),
					cmd("remove", "Remove a working tree", "git"),
					cmd("prune", "Prune stale working tree info", "git"),
					cmd("lock", "Lock a working tree", "git"),
					cmd("unlock", "Unlock a working tree", "git"),
					cmd("move", "Move a working tree", "git"),
				},
			},
		},
	}
}
