package spec

// editorsCount is the PRD 18.11 category size (editors, pagers, and file
// viewers).
const editorsCount = 27

func catalogEditors() []*Spec {
	return []*Spec{
		{
			Name:        "bat",
			Description: "Cat clone with syntax highlighting",
			Icon:        "viewer",
			Options: []Option{
				optD("Show line numbers", "-n", "--number"),
				optD("Plain output", "-p", "--plain"),
				optVal("Force a language", "-l", "--language"),
				optVal("Set the output style", "--style"),
				optVal("Show only a line range", "-r", "--line-range"),
			},
			Generator: "files",
		},
		{
			Name:        "cat",
			Description: "Concatenate and print files",
			Icon:        "viewer",
			Options: []Option{
				optD("Number all output lines", "-n"),
				optD("Number non-empty output lines", "-b"),
				optD("Show all non-printing characters", "-A"),
			},
			Generator: "files",
		},
		{
			Name:        "code",
			Description: "Visual Studio Code",
			Icon:        "editor",
			Options: []Option{
				optD("Wait for the window to close", "-w", "--wait"),
				optD("Open a new window", "-n", "--new-window"),
				optD("Reuse an existing window", "-r", "--reuse-window"),
				optD("Compare two files", "-d", "--diff"),
				optD("Open at a line and column", "-g", "--goto"),
				optVal("Install an extension", "--install-extension"),
			},
			Generator: "files",
		},
		genCmd("cot", "CotEditor", "editor", "files"),
		{
			Name:        "du",
			Description: "Estimate file space usage",
			Icon:        "fs",
			Options: []Option{
				optD("Human-readable sizes", "-h"),
				optD("Show only a total", "-s"),
				optVal("Limit the tree depth", "-d"),
				optD("Produce a grand total", "-c"),
			},
			Generator: "dirs",
		},
		{
			Name:        "emacs",
			Description: "GNU Emacs editor",
			Icon:        "editor",
			Options: []Option{
				optD("Run in the terminal", "-nw"),
				optD("Start without an init file", "-q"),
			},
			Generator: "files",
		},
		{
			Name:        "file",
			Description: "Determine file types",
			Icon:        "fs",
			Options: []Option{
				optD("Brief output", "-b"),
				optD("Show the MIME type", "-i"),
			},
			Generator: "files",
		},
		{
			Name:        "glow",
			Description: "Render markdown in the terminal",
			Icon:        "viewer",
			Options: []Option{
				optD("Page through the output", "-p", "--pager"),
				optVal("Set the output width", "-w", "--width"),
			},
			Generator: "files",
		},
		{
			Name:        "head",
			Description: "Print the first lines of files",
			Icon:        "viewer",
			Options: []Option{
				optVal("Print the first n lines", "-n"),
				optVal("Print the first n bytes", "-c"),
				optD("Never print file name headers", "-q"),
			},
			Generator: "files",
		},
		genCmd("idea", "IntelliJ IDEA", "editor", "files"),
		{
			Name:        "less",
			Description: "Page through files",
			Icon:        "viewer",
			Options: []Option{
				optD("Show line numbers", "-N"),
				optD("Chop long lines", "-S"),
				optD("Case-insensitive search", "-i"),
				optD("Pass through ANSI colors", "-R"),
				optD("Quit if the file fits on one screen", "-F"),
			},
			Generator: "files",
		},
		{
			Name:        "lvim",
			Description: "LunarVim editor",
			Icon:        "editor",
			Options: []Option{
				optD("Open in read-only mode", "-R"),
				optD("Open files in tabs", "-p"),
			},
			Generator: "files",
		},
		genCmd("micro", "Micro terminal editor", "editor", "files"),
		genCmd("more", "Page through files", "viewer", "files"),
		{
			Name:        "nano",
			Description: "Nano terminal editor",
			Icon:        "editor",
			Options: []Option{
				optD("Show line numbers", "-l", "--linenumbers"),
				optD("Auto-indent new lines", "-i", "--autoindent"),
				optD("Do not wrap long lines", "-w", "--nowrap"),
			},
			Generator: "files",
		},
		{
			Name:        "nvim",
			Description: "Neovim editor",
			Icon:        "editor",
			Options: []Option{
				optD("Open in read-only mode", "-R"),
				optD("Open files in tabs", "-p"),
				optD("Open files in horizontal splits", "-o"),
				optD("Open files in vertical splits", "-O"),
			},
			Generator: "files",
		},
		genCmd("rich", "Rich text and formatting in the terminal", "viewer", "files"),
		{
			Name:        "stat",
			Description: "Show file status",
			Icon:        "fs",
			Options: []Option{
				optVal("Format the output", "-f"),
			},
			Generator: "files",
		},
		{
			Name:        "subl",
			Description: "Sublime Text",
			Icon:        "editor",
			Options: []Option{
				optD("Wait for the files to close", "-w", "--wait"),
				optD("Open a new window", "-n", "--new-window"),
			},
			Generator: "files",
		},
		{
			Name:        "tail",
			Description: "Print the last lines of files",
			Icon:        "viewer",
			Options: []Option{
				optVal("Print the last n lines", "-n"),
				optD("Follow the file", "-f"),
				optD("Follow by name, retrying", "-F"),
				optVal("Print the last n bytes", "-c"),
			},
			Generator: "files",
		},
		{
			Name:        "vi",
			Description: "Vi editor",
			Icon:        "editor",
			Options: []Option{
				optD("Open in read-only mode", "-R"),
			},
			Generator: "files",
		},
		{
			Name:        "vim",
			Description: "Vi IMproved editor",
			Icon:        "editor",
			Options: []Option{
				optD("Open in read-only mode", "-R"),
				optD("Open files in tabs", "-p"),
				optD("Open files in horizontal splits", "-o"),
				optD("Open files in vertical splits", "-O"),
			},
			Generator: "files",
		},
		genCmd("vimr", "MacVim-based Vim GUI", "editor", "files"),
		{
			Name:        "wc",
			Description: "Count lines, words, and bytes",
			Icon:        "fs",
			Options: []Option{
				optD("Count lines", "-l"),
				optD("Count words", "-w"),
				optD("Count bytes", "-c"),
				optD("Count characters", "-m"),
			},
			Generator: "files",
		},
		genCmd("xed", "Xcode text editor", "editor", "files"),
		{
			Name:        "xxd",
			Description: "Hex dump and reverse",
			Icon:        "viewer",
			Options: []Option{
				optD("Reverse a hex dump", "-r"),
				optD("Plain hex dump", "-p"),
				optVal("Bytes per line", "-c"),
				optVal("Limit the length", "-l"),
			},
			Generator: "files",
		},
		genCmd("zed", "Zed editor", "editor", "files"),
	}
}
