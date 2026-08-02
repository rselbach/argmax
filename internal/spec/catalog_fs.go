package spec

// fsCount is the PRD 18.10 category size (filesystem, directory, and
// archive utilities).
const fsCount = 30

func catalogFS() []*Spec {
	return []*Spec{
		{
			Name:        "broot",
			Description: "Interactive directory tree navigator",
			Icon:        "fs",
			Options: []Option{
				optD("Show sizes", "-s", "--sizes"),
				optD("Show hidden files", "-h", "--hidden"),
			},
			Generator: "dirs",
		},
		genCmd("cd", "Change the working directory", "fs", "dirs"),
		{
			Name:        "chmod",
			Description: "Change file mode bits",
			Icon:        "fs",
			Options: []Option{
				optD("Recurse into directories", "-R", "--recursive"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Report only when a change is made", "-c", "--changes"),
			},
			Generator: "chmod-modes", // first positional; then files
			MaxArgs:   0,             // unlimited
		},
		{
			Name:        "chown",
			Description: "Change file ownership",
			Icon:        "fs",
			Options: []Option{
				optD("Recurse into directories", "-R", "--recursive"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Report only when a change is made", "-c", "--changes"),
			},
		},
		{
			Name:        "cp",
			Description: "Copy files and directories",
			Icon:        "fs",
			Options: []Option{
				optD("Copy directories recursively", "-r", "-R", "--recursive"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Prompt before overwriting", "-i", "--interactive"),
				optD("Overwrite without prompting", "-f", "--force"),
				optD("Preserve file attributes", "-p", "--preserve"),
				optD("Do not overwrite existing files", "-n", "--no-clobber"),
				optD("Archive mode", "-a", "--archive"),
			},
			Generator: "files",
		},
		{
			Name:        "df",
			Description: "Report file system disk space usage",
			Icon:        "fs",
			Options: []Option{
				optD("Human-readable sizes", "-h"),
				optD("Show inode usage", "-i"),
				optD("Show the file system type", "-T"),
			},
			Generator: "dirs",
		},
		{
			Name:        "dust",
			Description: "Intuitive disk usage tool",
			Icon:        "fs",
			Options: []Option{
				optVal("Limit the tree depth", "-d", "--depth"),
				optVal("Number of lines to show", "-n", "--number-of-lines"),
				optD("Reverse the tree", "-r", "--reverse"),
			},
			Generator: "dirs",
		},
		{
			Name:        "exa",
			Description: "Modern replacement for ls",
			Icon:        "fs",
			Options: []Option{
				optD("Long listing", "-l", "--long"),
				optD("Show hidden files", "-a", "--all"),
				optD("Show a tree", "-T", "--tree"),
				optVal("Limit the tree depth", "-L", "--level"),
				optD("Show icons", "--icons"),
			},
			Generator: "dirs",
		},
		{
			Name:        "eza",
			Description: "Modern replacement for ls (exa fork)",
			Icon:        "fs",
			Options: []Option{
				optD("Long listing", "-l", "--long"),
				optD("Show hidden files", "-a", "--all"),
				optD("Show a tree", "-T", "--tree"),
				optVal("Limit the tree depth", "-L", "--level"),
				optD("Show icons", "--icons"),
			},
			Generator: "dirs",
		},
		// find is cross-listed in PRD 18.10 and 18.12; this placement carries
		// the directory generator and traversal predicates, the text
		// placement the name/exec predicates, and the registry merges both.
		{
			Name:        "find",
			Description: "Search for files in a directory hierarchy",
			Icon:        "search",
			Options: []Option{
				optVal("Filter by file type", "-type"),
				optVal("Limit the search depth", "-maxdepth"),
				optVal("Minimum search depth", "-mindepth"),
				optVal("Filter by modification time", "-mtime"),
				optVal("Filter by size", "-size"),
			},
			Generator: "dirs",
		},
		{
			Name:        "fold",
			Description: "Wrap input lines to a width",
			Icon:        "text",
			Options: []Option{
				optVal("Wrap at the given width", "-w"),
				optD("Break at spaces", "-s"),
				optD("Count bytes instead of columns", "-b"),
			},
			Generator: "files",
		},
		{
			Name:        "ln",
			Description: "Create links between files",
			Icon:        "fs",
			Options: []Option{
				optD("Create a symbolic link", "-s"),
				optD("Overwrite existing links", "-f"),
				optD("Verbose output", "-v"),
				optD("Do not follow an existing directory symlink", "-n"),
			},
			Generator: "files",
		},
		{
			Name:        "ls",
			Description: "List directory contents",
			Icon:        "fs",
			Options: []Option{
				optD("Long listing", "-l"),
				optD("Show hidden files", "-a"),
				optD("Human-readable sizes", "-h"),
				optD("Recurse into directories", "-R"),
				optD("Sort by modification time", "-t"),
				optD("Sort by size", "-S"),
			},
			Generator: "dirs",
		},
		{
			Name:        "lsd",
			Description: "Next-generation ls",
			Icon:        "fs",
			Options: []Option{
				optD("Long listing", "-l"),
				optD("Show hidden files", "-a", "--all"),
				optD("Show a tree", "--tree"),
				optD("List directories themselves", "-d", "--directory-only"),
			},
			Generator: "dirs",
		},
		{
			Name:        "mkdir",
			Description: "Create directories",
			Icon:        "fs",
			Options: []Option{
				optD("Create parent directories as needed", "-p", "--parents"),
				optD("Verbose output", "-v", "--verbose"),
			},
			Generator: "dirs",
		},
		{
			Name:        "mv",
			Description: "Move or rename files",
			Icon:        "fs",
			Options: []Option{
				optD("Prompt before overwriting", "-i", "--interactive"),
				optD("Overwrite without prompting", "-f", "--force"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Do not overwrite existing files", "-n", "--no-clobber"),
			},
			Generator: "files",
		},
		cmd("paper", "Paper document CLI", "fs"),
		{
			Name:        "rclone",
			Description: "Sync files with cloud storage",
			Icon:        "cloud",
			Subcommands: []*Spec{
				cmd("copy", "Copy files", "cloud"),
				cmd("sync", "Synchronize directories", "cloud"),
				cmd("move", "Move files", "cloud"),
				cmd("ls", "List objects", "cloud"),
				cmd("mount", "Mount a remote", "cloud"),
				cmd("config", "Manage remotes", "cloud"),
				cmd("delete", "Delete files", "cloud"),
			},
		},
		{
			Name:        "readlink",
			Description: "Print resolved symbolic links",
			Icon:        "fs",
			Options: []Option{
				optD("Canonicalize the path", "-f"),
				optD("Canonicalize without requiring existence", "-m"),
			},
			Generator: "files",
		},
		{
			Name:        "rm",
			Description: "Remove files and directories",
			Icon:        "fs",
			Options: []Option{
				optD("Remove directories recursively", "-r", "-R", "--recursive"),
				optD("Remove without prompting", "-f", "--force"),
				optD("Prompt before every removal", "-i", "--interactive"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Remove empty directories", "-d", "--dir"),
			},
			Generator: "files",
		},
		{
			Name:        "rmdir",
			Description: "Remove empty directories",
			Icon:        "fs",
			Options: []Option{
				optD("Remove parent directories as needed", "-p", "--parents"),
			},
			Generator: "dirs",
		},
		{
			Name:        "stow",
			Description: "Symlink farm manager",
			Icon:        "package",
			Options: []Option{
				optGen("dirs", "Stow directory", "-d", "--dir"),
				optGen("dirs", "Target directory", "-t", "--target"),
				optD("Stow the packages", "-S", "--stow"),
				optD("Unstow the packages", "-D", "--delete"),
				optD("Restow the packages", "-R", "--restow"),
				optD("Simulate the operations", "-n", "--simulate"),
				optD("Verbose output", "-v", "--verbose"),
			},
		},
		{
			Name:        "tar",
			Description: "Archive files",
			Icon:        "archive",
			Options: []Option{
				optD("Create an archive", "-c"),
				optD("Extract an archive", "-x"),
				optD("List archive contents", "-t"),
				optD("Filter through gzip", "-z"),
				optD("Filter through bzip2", "-j"),
				optD("Verbose output", "-v"),
				optGen("files", "Archive file", "-f"),
				optGen("dirs", "Change to directory", "-C"),
			},
			Generator: "files",
		},
		{
			Name:        "touch",
			Description: "Update file timestamps",
			Icon:        "fs",
			Options: []Option{
				optD("Update the access time", "-a"),
				optD("Update the modification time", "-m"),
				optD("Do not create the file", "-c"),
			},
			Generator: "files",
		},
		cmd("trash", "Move files to the trash", "fs"),
		{
			Name:        "tree",
			Description: "List directories as a tree",
			Icon:        "fs",
			Options: []Option{
				optVal("Limit the display depth", "-L"),
				optD("List directories only", "-d"),
				optD("Show hidden files", "-a"),
				optVal("Exclude matching paths", "-I"),
				optD("Human-readable sizes", "-h"),
			},
			Generator: "dirs",
		},
		{
			Name:        "unzip",
			Description: "Extract ZIP archives",
			Icon:        "archive",
			Options: []Option{
				optGen("dirs", "Extract into directory", "-d"),
				optD("List archive contents", "-l"),
				optD("Overwrite without prompting", "-o"),
				optD("Quiet mode", "-q"),
			},
			Generator: "ext:zip",
		},
		genCmd("z", "Jump to a frecent directory", "fs", "zoxide-dirs"),
		genCmd("zi", "Interactively pick a frecent directory", "fs", "zoxide-dirs"),
		{
			Name:        "zip",
			Description: "Create ZIP archives",
			Icon:        "archive",
			Options: []Option{
				optD("Recurse into directories", "-r"),
				optD("Encrypt the archive", "-e"),
				optD("Maximum compression", "-9"),
				optVal("Delete entries from the archive", "-d"),
			},
			Generator: "files",
		},
	}
}
