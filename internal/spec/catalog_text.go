package spec

// textCount is the PRD 18.12 category size (text, JSON, and stream
// processing).
const textCount = 28

func catalogText() []*Spec {
	return []*Spec{
		{
			Name:        "awk",
			Description: "Pattern scanning and processing language",
			Icon:        "text",
			Options: []Option{
				optVal("Field separator", "-F"),
				optVal("Set a variable", "-v"),
				optGen("files", "Read the program from a file", "-f"),
			},
			Generator: "files",
		},
		{
			Name:        "cut",
			Description: "Cut out selected fields of lines",
			Icon:        "text",
			Options: []Option{
				optVal("Field delimiter", "-d"),
				optVal("Select fields", "-f"),
				optVal("Select characters", "-c"),
			},
			Generator: "files",
		},
		{
			Name:        "diff",
			Description: "Compare files line by line",
			Icon:        "text",
			Options: []Option{
				optD("Unified format", "-u"),
				optD("Compare directories recursively", "-r"),
				optD("Report only whether files differ", "-q"),
				optD("Side-by-side format", "-y"),
				optD("Ignore case", "-i"),
				optD("Ignore whitespace", "-w"),
			},
			Generator: "files",
		},
		genCmd("dos2unix", "Convert DOS line endings to Unix", "text", "files"),
		grepFamilySpec("egrep", "Search with extended regular expressions"),
		{
			Name:        "fd",
			Description: "Fast alternative to find",
			Icon:        "search",
			Options: []Option{
				optVal("Filter by extension", "-e", "--extension"),
				optVal("Filter by type", "-t", "--type"),
				optD("Include hidden files", "-H", "--hidden"),
				optD("Include ignored files", "-I", "--no-ignore"),
				optVal("Limit the search depth", "-d", "--max-depth"),
				optVal("Exclude matching paths", "-E", "--exclude"),
				optVal("Execute a command per result", "-x", "--exec"),
			},
			Generator: "dirs",
		},
		// find is cross-listed in PRD 18.10 and 18.12; this placement carries
		// the name/exec predicates, the filesystem placement the directory
		// generator and traversal predicates. The registry merges both into
		// one spec.
		{
			Name:        "find",
			Description: "Search for files in a directory hierarchy",
			Icon:        "search",
			Options: []Option{
				optVal("Match the file name", "-name"),
				optVal("Match the file name, ignoring case", "-iname"),
				optD("Execute a command per result", "-exec"),
				optD("Delete matching files", "-delete"),
			},
		},
		{
			Name:        "gawk",
			Description: "GNU awk",
			Icon:        "text",
			Options: []Option{
				optVal("Field separator", "-F"),
				optVal("Set a variable", "-v"),
				optGen("files", "Read the program from a file", "-f"),
			},
			Generator: "files",
		},
		grepFamilySpec("grep", "Search for patterns in files"),
		{
			Name:        "iconv",
			Description: "Convert text between encodings",
			Icon:        "text",
			Options: []Option{
				optVal("Convert from encoding", "-f", "--from-code"),
				optVal("Convert to encoding", "-t", "--to-code"),
				optGen("files", "Write output to file", "-o", "--output"),
			},
			Generator: "files",
		},
		{
			Name:        "jq",
			Description: "JSON processor",
			Icon:        "json",
			Options: []Option{
				optD("Raw string output", "-r", "--raw-output"),
				optD("Compact output", "-c", "--compact-output"),
				optD("Set the exit status from the output", "-e", "--exit-status"),
				optGen("files", "Read the filter from a file", "-f", "--from-file"),
				optVal("Set a string variable", "--arg"),
				optD("Read no input", "-n", "--null-input"),
			},
			Generator: "files",
		},
		{
			Name:        "pandoc",
			Description: "Universal document converter",
			Icon:        "text",
			Options: []Option{
				optVal("Input format", "-f", "--from"),
				optVal("Output format", "-t", "--to"),
				optGen("files", "Output file", "-o", "--output"),
				optD("Produce a standalone document", "-s", "--standalone"),
			},
			Generator: "files",
		},
		{
			Name:        "rg",
			Description: "Recursively search with ripgrep",
			Icon:        "search",
			Options: []Option{
				optD("Ignore case", "-i", "--ignore-case"),
				optD("Invert the match", "-v", "--invert-match"),
				optD("Show line numbers", "-n", "--line-number"),
				optD("Show file names only", "-l", "--files-with-matches"),
				optD("Match whole words", "-w", "--word-regexp"),
				optVal("Show lines after each match", "-A", "--after-context"),
				optVal("Show lines before each match", "-B", "--before-context"),
				optVal("Show lines around each match", "-C", "--context"),
				optVal("Use the given pattern", "-e", "--regexp"),
				optVal("Filter by file type", "-t", "--type"),
				optVal("Include or exclude files by glob", "-g", "--glob"),
				optD("Search hidden files", "--hidden"),
				optD("Smart case", "-S", "--smart-case"),
				optD("Match literally", "-F", "--fixed-strings"),
			},
			Generator: "files",
		},
		{
			Name:        "sed",
			Description: "Stream editor",
			Icon:        "text",
			Options: []Option{
				optD("Edit files in place", "-i"),
				optD("Suppress automatic printing", "-n"),
				optVal("Add a script command", "-e"),
				optD("Use extended regular expressions", "-E", "-r"),
			},
			Generator: "files",
		},
		{
			Name:        "seq",
			Description: "Print sequences of numbers",
			Icon:        "text",
			Options: []Option{
				optVal("Number separator", "-s"),
				optD("Equalize widths with leading zeros", "-w"),
			},
		},
		{
			Name:        "sha1sum",
			Description: "Compute SHA-1 checksums",
			Icon:        "text",
			Options: []Option{
				optD("Verify checksums from a file", "-c", "--check"),
			},
			Generator: "files",
		},
		{
			Name:        "shasum",
			Description: "Compute SHA checksums",
			Icon:        "text",
			Options: []Option{
				optVal("SHA algorithm", "-a"),
				optD("Verify checksums from a file", "-c", "--check"),
			},
			Generator: "files",
		},
		{
			Name:        "shred",
			Description: "Securely delete files",
			Icon:        "text",
			Options: []Option{
				optD("Remove the file after shredding", "-u"),
				optVal("Overwrite n times", "-n"),
				optD("Overwrite with zeros last", "-z"),
				optD("Force permissions to allow writing", "-f"),
			},
			Generator: "files",
		},
		{
			Name:        "sort",
			Description: "Sort lines of text",
			Icon:        "text",
			Options: []Option{
				optD("Sort numerically", "-n"),
				optD("Reverse the order", "-r"),
				optVal("Sort by a key", "-k"),
				optD("Drop duplicate lines", "-u"),
				optVal("Field separator", "-t"),
				optGen("files", "Write output to file", "-o"),
			},
			Generator: "files",
		},
		{
			Name:        "split",
			Description: "Split files into pieces",
			Icon:        "text",
			Options: []Option{
				optVal("Lines per piece", "-l"),
				optVal("Bytes per piece", "-b"),
				optD("Use numeric suffixes", "-d"),
			},
			Generator: "files",
		},
		{
			Name:        "tee",
			Description: "Duplicate standard input",
			Icon:        "text",
			Options: []Option{
				optD("Append instead of overwriting", "-a"),
				optD("Ignore interrupts", "-i"),
			},
			Generator: "files",
		},
		{
			Name:        "tr",
			Description: "Translate or delete characters",
			Icon:        "text",
			Options: []Option{
				optD("Delete characters", "-d"),
				optD("Squeeze repeated characters", "-s"),
				optD("Complement the first set", "-c"),
			},
		},
		{
			Name:        "truncate",
			Description: "Shrink or extend the size of files",
			Icon:        "text",
			Options: []Option{
				optVal("Set the file size", "-s"),
			},
			Generator: "files",
		},
		{
			Name:        "typos",
			Description: "Source code spell checker",
			Icon:        "text",
			Options: []Option{
				optD("Write corrections", "-w", "--write-changes"),
			},
			Generator: "files",
		},
		{
			Name:        "uniq",
			Description: "Filter adjacent duplicate lines",
			Icon:        "text",
			Options: []Option{
				optD("Prefix lines with occurrence counts", "-c"),
				optD("Show only duplicate lines", "-d"),
				optD("Show only unique lines", "-u"),
				optD("Ignore case", "-i"),
			},
			Generator: "files",
		},
		genCmd("unix2dos", "Convert Unix line endings to DOS", "text", "files"),
		{
			Name:        "vale",
			Description: "Prose linter",
			Icon:        "text",
			Options: []Option{
				optGen("files", "Use a configuration file", "--config"),
			},
			Generator: "files",
		},
		{
			Name:        "xargs",
			Description: "Build and execute command lines from input",
			Icon:        "text",
			Options: []Option{
				optD("Input items are NUL-separated", "-0"),
				optVal("Replace a string with the input item", "-I"),
				optVal("Max arguments per command", "-n"),
				optVal("Run commands in parallel", "-P"),
				optD("Print commands before running", "-t"),
				optD("Prompt before running", "-p"),
			},
		},
	}
}

// grepFamilySpec builds the shared grep/egrep/ag-style spec.
func grepFamilySpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "search",
		Options: []Option{
			optD("Ignore case", "-i"),
			optD("Invert the match", "-v"),
			optD("Recurse into directories", "-r"),
			optD("Show line numbers", "-n"),
			optD("Show file names only", "-l"),
			optD("Match whole words", "-w"),
			optVal("Show lines after each match", "-A"),
			optVal("Show lines before each match", "-B"),
			optVal("Show lines around each match", "-C"),
			optVal("Use the given pattern", "-e"),
		},
		Generator: "files",
	}
}
