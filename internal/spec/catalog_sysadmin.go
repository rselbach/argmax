package spec

// sysadminCount is the PRD 18.14 category size (system administration,
// network, and process management).
const sysadminCount = 175

func catalogSysadmin() []*Spec {
	specs := []*Spec{
		agSpec(),
		atuinSpec(),
		btopSpec(),
		chezmoiSpec(),
		crontabSpec(),
		curlSpec(),
		defaultsSpec(),
		envSpec(),
		exportSpec(),
		ffmpegSpec(),
		fzfSpec(),
		htopSpec(),
		killallSpec(),
		killSpec(),
		launchctlSpec(),
		manSpec(),
		moshSpec(),
		printenvSpec(),
		psSpec(),
		screenSpec(),
		sudoSpec(),
		systemctlSpec(),
		tldrSpec(),
		tmuxSpec(),
		topSpec(),
		unsetSpec(),
		wgetSpec(),
		whereSpec(),
		whereisSpec(),
		whichSpec(),
	}
	for _, e := range sysadminSimple {
		specs = append(specs, cmd(e[0], e[1], e[2]))
	}
	return specs
}

// sysadminSimple holds the single-level entries of PRD 18.14.
var sysadminSimple = [][3]string{
	{"adb", "Android Debug Bridge", "sysadmin"},
	{"airflow", "Apache Airflow workflow platform", "task"},
	{"aliases", "Manage shell aliases", "shell"},
	{"asciinema", "Record terminal sessions", "shell"},
	{"asr", "Apple Software Restore", "sysadmin"},
	{"basename", "Strip the directory from a path", "fs"},
	{"bc", "Arbitrary precision calculator", "misc"},
	{"bundle", "Ruby dependency manager", "package"},
	{"cal", "Display a calendar", "misc"},
	{"cci", "CircleCI CLI", "cloud"},
	{"cdk8s", "Kubernetes manifests as code", "kubernetes"},
	{"chsh", "Change the login shell", "sysadmin"},
	{"codesign", "Sign and verify macOS code", "sysadmin"},
	{"croc", "Transfer files between computers", "network"},
	{"date", "Display or set the date and time", "sysadmin"},
	{"dateseq", "Generate sequences of dates", "misc"},
	{"dcli", "Dashlane CLI", "misc"},
	{"dd", "Convert and copy data blocks", "fs"},
	{"ddev", "Local PHP development environments", "cloud"},
	{"degit", "Clone repositories without history", "git"},
	{"deta", "Deta cloud CLI", "cloud"},
	{"dig", "DNS lookup utility", "network"},
	{"dirname", "Strip the filename from a path", "fs"},
	{"do-release-upgrade", "Upgrade the Ubuntu release", "sysadmin"},
	{"dog", "Modern DNS client", "network"},
	{"dotnet", ".NET SDK CLI", "build"},
	{"dscacheutil", "Query the macOS Directory Service cache", "sysadmin"},
	{"dscl", "macOS Directory Service CLI", "sysadmin"},
	{"dtm", "macOS device management CLI", "sysadmin"},
	{"echo", "Write arguments to standard output", "shell"},
	{"eleventy", "Static site generator", "node"},
	{"exec", "Replace the shell with a command", "shell"},
	{"fastlane", "Mobile app build automation", "build"},
	{"fdisk", "Partition table manipulator", "sysadmin"},
	{"firefox", "Firefox browser", "misc"},
	{"fisher", "Fish plugin manager", "shell"},
	{"fmt", "Reformat paragraph text", "text"},
	{"forc", "Fuel orchestrator CLI", "cloud"},
	{"forge", "Foundry Ethereum toolkit", "build"},
	{"fzf-tmux", "Open fzf in a tmux pane", "search"},
	{"gltfjsx", "Convert GLTF models to JSX", "node"},
	{"goto", "Directory bookmarking tool", "fs"},
	{"gum", "Glamorous shell script components", "shell"},
	{"herd", "Laravel Herd CLI", "misc"},
	{"hop", "Secret management CLI", "misc"},
	{"hostname", "Show or set the host name", "network"},
	{"http", "HTTPie API client", "network"},
	{"hyper", "Hyper terminal", "shell"},
	{"hyperfine", "Command-line benchmarking", "task"},
	{"ibus", "Input method framework CLI", "sysadmin"},
	{"id", "Print user and group IDs", "sysadmin"},
	{"ifconfig", "Configure network interfaces", "network"},
	{"ignite-cli", "Cosmos blockchain scaffold", "cloud"},
	{"install", "Copy files and set attributes", "fs"},
	{"ip", "Show and manipulate routing and devices", "network"},
	{"join", "Join lines of two files on a field", "text"},
	{"julia", "Julia language runtime", "misc"},
	{"kafkactl", "Kafka cluster management CLI", "cloud"},
	{"kamal", "Deploy web apps with Docker", "docker"},
	{"kitty", "Kitty terminal emulator", "shell"},
	{"klist", "List Kerberos tickets", "sysadmin"},
	{"kool", "Development environment CLI", "cloud"},
	{"leaf", "Leaf package manager", "package"},
	{"lima", "Linux virtual machines on macOS", "cloud"},
	{"login", "Log into the system", "sysadmin"},
	{"lsblk", "List block devices", "sysadmin"},
	{"lsof", "List open files", "sysadmin"},
	{"meroxa", "Data streaming platform CLI", "cloud"},
	{"mkdocs", "Documentation site generator", "build"},
	{"mkfifo", "Create named pipes", "fs"},
	{"mkinitcpio", "Generate an initramfs", "sysadmin"},
	{"mknod", "Create special files", "sysadmin"},
	{"mount", "Mount file systems", "sysadmin"},
	{"nc", "Read and write network connections", "network"},
	{"ncal", "Display a calendar", "misc"},
	{"neofetch", "Show system information", "sysadmin"},
	{"netstat", "Show network connections", "network"},
	{"networkQuality", "Measure network quality", "network"},
	{"networksetup", "Configure macOS networking", "network"},
	{"nextflow", "Data-driven workflow engine", "task"},
	{"nhost", "Nhost backend CLI", "cloud"},
	{"nmap", "Network exploration and scanning", "network"},
	{"nrm", "npm registry manager", "node"},
	{"ns", "NativeScript CLI", "node"},
	{"nslookup", "Query DNS servers", "network"},
	{"nylas", "Nylas API CLI", "cloud"},
	{"oh-my-posh", "Custom shell prompt", "shell"},
	{"okta", "Okta identity CLI", "cloud"},
	{"ollama", "Run local language models", "misc"},
	{"omz", "Oh My Zsh CLI", "shell"},
	{"pac", "Power Platform CLI", "cloud"},
	{"passwd", "Change the user password", "sysadmin"},
	{"pathchk", "Check path validity and portability", "fs"},
	{"pdfunite", "Merge PDF documents", "misc"},
	{"pgrep", "Find processes by name", "process"},
	{"ping", "Send ICMP echo requests", "network"},
	{"pkg-config", "Query library compile flags", "build"},
	{"pkill", "Signal processes by name", "process"},
	{"pmset", "macOS power management settings", "sysadmin"},
	{"pocketbase", "Backend in a single file", "database"},
	{"prisma", "Database ORM CLI", "database"},
	{"pro", "Process management CLI", "process"},
	{"pry", "Ruby interactive console", "shell"},
	{"publish", "Swift static site generator", "build"},
	{"pwd", "Print the working directory", "fs"},
	{"rancher", "Rancher container management CLI", "kubernetes"},
	{"repeat", "Repeat a command", "task"},
	{"rscript", "Run R scripts", "misc"},
	{"sam", "AWS Serverless Application Model CLI", "cloud"},
	{"sanity", "Sanity CMS CLI", "cloud"},
	{"shell-config", "Edit shell configuration", "shell"},
	{"shortcuts", "Run macOS Shortcuts", "sysadmin"},
	{"simctl", "Manage iOS simulators", "build"},
	{"source", "Execute commands in the current shell", "shell"},
	{"speedtest-cli", "Test internet bandwidth", "network"},
	{"spotify", "Spotify CLI", "misc"},
	{"ss", "Investigate sockets", "network"},
	{"st2", "StackStorm automation CLI", "task"},
	{"stack", "Haskell tool stack", "build"},
	{"starkli", "Starknet CLI", "cloud"},
	{"su", "Substitute user identity", "sysadmin"},
	{"sysctl", "Get and set kernel state", "sysadmin"},
	{"tac", "Print files in reverse", "text"},
	{"tailcall", "GraphQL platform CLI", "cloud"},
	{"tailwindcss", "Utility-first CSS framework CLI", "node"},
	{"time", "Time command execution", "task"},
	{"tmuxinator", "Manage tmux sessions", "shell"},
	{"traceroute", "Trace the route to a host", "network"},
	{"trap", "Trap shell signals", "shell"},
	{"trex", "t-rex tile server", "cloud"},
	{"tsh", "Teleport client", "network"},
	{"tuist", "Xcode project generation tool", "build"},
	{"twilio", "Twilio API CLI", "cloud"},
	{"uname", "Print system information", "sysadmin"},
	{"visudo", "Edit the sudoers file safely", "sysadmin"},
	{"vultr-cli", "Vultr cloud CLI", "cloud"},
	{"wezterm", "WezTerm terminal emulator", "shell"},
	{"who", "Show who is logged in", "sysadmin"},
	{"wing", "The Silver Searcher GUI", "search"},
	{"wp", "WordPress CLI", "misc"},
	{"wrk", "HTTP benchmarking tool", "network"},
	{"wscat", "WebSocket CLI", "network"},
	{"yank", "Yank terminal output to the clipboard", "misc"},
	{"ykman", "YubiKey manager", "sysadmin"},
	{"zapier", "Zapier automation CLI", "cloud"},
}

// agSpec builds the silver searcher spec (grep family, PRD 9.5 options).
func agSpec() *Spec {
	return &Spec{
		Name:        "ag",
		Description: "The Silver Searcher code search tool",
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

// atuinSpec builds the atuin shell history spec.
func atuinSpec() *Spec {
	return &Spec{
		Name:        "atuin",
		Description: "Magical shell history",
		Icon:        "shell",
		Subcommands: []*Spec{
			cmd("search", "Search the history", "shell"),
			cmd("history", "Manage history entries", "shell"),
			cmd("stats", "Show history statistics", "shell"),
			cmd("sync", "Synchronize the history", "shell"),
			cmd("login", "Log in to the sync server", "shell"),
			cmd("register", "Register a sync account", "shell"),
			cmd("import", "Import existing shell history", "shell"),
		},
	}
}

// btopSpec builds the btop resource monitor spec.
func btopSpec() *Spec {
	return &Spec{
		Name:        "btop",
		Description: "Resource monitor",
		Icon:        "process",
		Options: []Option{
			optD("Show version", "-v", "--version"),
			optD("Start in TTY mode", "-t", "--tty_on"),
		},
	}
}

// chezmoiSpec builds the chezmoi dotfile manager spec.
func chezmoiSpec() *Spec {
	return &Spec{
		Name:        "chezmoi",
		Description: "Dotfile manager",
		Icon:        "fs",
		Subcommands: []*Spec{
			{Name: "add", Description: "Add a file to the source state", Icon: "fs", Generator: "files"},
			cmd("apply", "Apply the target state", "fs"),
			cmd("diff", "Show the pending changes", "fs"),
			cmd("edit", "Edit a source file", "fs"),
			cmd("init", "Initialize the source directory", "fs"),
			cmd("status", "Show the status", "fs"),
			cmd("update", "Pull and apply changes", "fs"),
			cmd("cd", "Open a shell in the source directory", "fs"),
		},
	}
}

// crontabSpec builds the crontab spec.
func crontabSpec() *Spec {
	return &Spec{
		Name:        "crontab",
		Description: "Maintain crontab files",
		Icon:        "task",
		Options: []Option{
			optD("Edit the crontab", "-e"),
			optD("List the crontab", "-l"),
			optD("Delete the crontab", "-r"),
			optVal("Operate on a user's crontab", "-u"),
			optD("Prompt before deleting", "-i"),
		},
	}
}

// curlSpec builds the curl spec (PRD 9.5 option list).
func curlSpec() *Spec {
	return &Spec{
		Name:        "curl",
		Description: "Transfer data with URLs",
		Icon:        "network",
		Options: []Option{
			optVal("HTTP method", "-X", "--request"),
			optVal("Request header", "-H", "--header"),
			optVal("Request body data", "-d", "--data"),
			optGen("files", "Write output to file", "-o", "--output"),
			optD("Write output to a file named like the remote", "-O", "--remote-name"),
			optD("Follow redirects", "-L", "--location"),
			optD("Silent mode", "-s", "--silent"),
			optD("Show errors in silent mode", "-S", "--show-error"),
			optD("Include response headers in the output", "-i", "--include"),
			optVal("Send JSON data", "--json"),
			optVal("URL-encode data", "--data-urlencode"),
			optVal("User name and password", "-u", "--user"),
			optD("Request a compressed response", "--compressed"),
			optD("Verbose output", "-v", "--verbose"),
		},
	}
}

// defaultsSpec builds the macOS defaults spec.
func defaultsSpec() *Spec {
	return &Spec{
		Name:        "defaults",
		Description: "Access macOS user defaults",
		Icon:        "sysadmin",
		Subcommands: []*Spec{
			cmd("read", "Read defaults", "sysadmin"),
			cmd("write", "Write defaults", "sysadmin"),
			cmd("delete", "Delete defaults", "sysadmin"),
			cmd("find", "Search defaults", "sysadmin"),
			cmd("domains", "List preference domains", "sysadmin"),
			cmd("export", "Export a domain", "sysadmin"),
			cmd("import", "Import a domain", "sysadmin"),
		},
	}
}

// envSpec builds the env spec.
func envSpec() *Spec {
	return &Spec{
		Name:        "env",
		Description: "Run a program in a modified environment",
		Icon:        "shell",
		Options: []Option{
			optD("Start with an empty environment", "-i"),
			optVal("Remove a variable", "-u"),
		},
		Generator: "env-vars",
	}
}

// exportSpec builds the export builtin spec.
func exportSpec() *Spec {
	return &Spec{
		Name:        "export",
		Description: "Set environment variables",
		Icon:        "shell",
		Options: []Option{
			optD("Refer to shell functions", "-f"),
			optD("Print exported names", "-p"),
		},
		Generator: "env-vars",
	}
}

// ffmpegSpec builds the ffmpeg spec.
func ffmpegSpec() *Spec {
	return &Spec{
		Name:        "ffmpeg",
		Description: "Audio and video converter",
		Icon:        "misc",
		Options: []Option{
			optGen("files", "Input file", "-i"),
			optVal("Video codec", "-c:v"),
			optVal("Audio codec", "-c:a"),
			optVal("Video bitrate", "-b:v"),
			optVal("Audio bitrate", "-b:a"),
			optVal("Start time offset", "-ss"),
			optVal("Duration limit", "-t"),
			optD("Overwrite output files", "-y"),
			optVal("Video filter graph", "-vf"),
			optVal("Force the format", "-f"),
		},
	}
}

// fzfSpec builds the fzf spec.
func fzfSpec() *Spec {
	return &Spec{
		Name:        "fzf",
		Description: "Fuzzy finder",
		Icon:        "search",
		Options: []Option{
			optD("Multi-select", "-m", "--multi"),
			optVal("Window height", "--height"),
			optVal("Preview command", "--preview"),
			optVal("Initial query", "-q", "--query"),
			optD("Exact match", "-e", "--exact"),
		},
	}
}

// htopSpec builds the htop process viewer spec.
func htopSpec() *Spec {
	return &Spec{
		Name:        "htop",
		Description: "Interactive process viewer",
		Icon:        "process",
		Options: []Option{
			optVal("Show only a user's processes", "-u", "--user"),
			optVal("Show only the given PIDs", "-p", "--pid"),
			optVal("Sort by a column", "-s", "--sort-key"),
		},
	}
}

// killSpec builds the kill spec; it completes process IDs.
func killSpec() *Spec {
	return &Spec{
		Name:        "kill",
		Description: "Send a signal to a process",
		Icon:        "process",
		Options: []Option{
			optD("Force kill (SIGKILL)", "-9"),
			optD("Terminate gracefully (SIGTERM)", "-15"),
			optD("List signal names", "-l"),
			optVal("Send a named signal", "-s"),
		},
		Generator: "processes",
	}
}

// killallSpec builds the killall spec; it completes process names.
func killallSpec() *Spec {
	return &Spec{
		Name:        "killall",
		Description: "Kill processes by name",
		Icon:        "process",
		Options: []Option{
			optD("Force kill (SIGKILL)", "-9"),
			optD("Terminate gracefully (SIGTERM)", "-15"),
		},
		Generator: "process-names",
	}
}

// launchctlSpec builds the launchd control spec.
func launchctlSpec() *Spec {
	return &Spec{
		Name:        "launchctl",
		Description: "Interface with launchd",
		Icon:        "sysadmin",
		Subcommands: []*Spec{
			cmd("load", "Load a job definition", "sysadmin"),
			cmd("unload", "Unload a job definition", "sysadmin"),
			cmd("start", "Start a job", "sysadmin"),
			cmd("stop", "Stop a job", "sysadmin"),
			cmd("list", "List jobs", "sysadmin"),
			cmd("print", "Print a job definition", "sysadmin"),
			cmd("kickstart", "Kickstart a service", "sysadmin"),
			cmd("bootstrap", "Bootstrap a service", "sysadmin"),
			cmd("bootout", "Boot out a service", "sysadmin"),
		},
	}
}

// manSpec builds the man spec.
func manSpec() *Spec {
	return &Spec{
		Name:        "man",
		Description: "Read manual pages",
		Icon:        "viewer",
		Options: []Option{
			optVal("Search by keyword", "-k"),
			optD("Show all matching pages", "-a"),
		},
	}
}

// moshSpec builds the mosh spec; it completes SSH hosts.
func moshSpec() *Spec {
	return &Spec{
		Name:        "mosh",
		Description: "Mobile shell",
		Icon:        "network",
		Options: []Option{
			optVal("Server-side UDP port", "-p", "--port"),
			optVal("SSH command to use", "--ssh"),
		},
		Generator: "ssh-hosts",
	}
}

// printenvSpec builds the printenv spec.
func printenvSpec() *Spec {
	return &Spec{
		Name:        "printenv",
		Description: "Print environment variables",
		Icon:        "shell",
		Options: []Option{
			optD("End lines with NUL", "-0", "--null"),
		},
		Generator: "env-vars",
	}
}

// psSpec builds the ps spec.
func psSpec() *Spec {
	return &Spec{
		Name:        "ps",
		Description: "Report process status",
		Icon:        "process",
		Options: []Option{
			optD("Show all processes in BSD style", "aux"),
			optVal("Select the output columns", "-eo"),
			optVal("Show the given PIDs", "-p"),
			optD("Show all processes, full format", "-ef"),
			optD("Show every process", "-e", "-A"),
			optD("Full format", "-f"),
			optVal("Show a user's processes", "-u"),
		},
	}
}

// screenSpec builds the screen terminal multiplexer spec.
func screenSpec() *Spec {
	return &Spec{
		Name:        "screen",
		Description: "Terminal multiplexer",
		Icon:        "shell",
		Options: []Option{
			optVal("Name the session", "-S"),
			optVal("Reattach to a session", "-r"),
			optD("List sessions", "-ls"),
			optD("Start detached", "-dm"),
			optVal("Send a command to a session", "-X"),
		},
	}
}

// sudoSpec builds the sudo spec.
func sudoSpec() *Spec {
	return &Spec{
		Name:        "sudo",
		Description: "Execute a command as another user",
		Icon:        "sysadmin",
		Options: []Option{
			optVal("Run as a user", "-u"),
			optD("Run a login shell", "-i"),
			optD("Run a shell", "-s"),
			optD("Preserve the environment", "-E"),
			optD("Validate the timestamp", "-v"),
			optD("Invalidate the timestamp", "-k"),
			optD("Do not prompt", "-n"),
			optD("Set the HOME variable", "-H"),
		},
	}
}

// systemctlSpec builds the systemd control spec. Units come from the AI
// context, so there is no live menu generator (PRD 9.8).
func systemctlSpec() *Spec {
	return &Spec{
		Name:        "systemctl",
		Description: "Control the systemd system and services",
		Icon:        "sysadmin",
		Options: []Option{
			optD("Operate on the user manager", "--user"),
			optD("Operate on the system manager", "--system"),
			optD("Also start or stop units now", "--now"),
			optD("Show all units", "-a", "--all"),
		},
		Subcommands: []*Spec{
			cmd("start", "Start units", "sysadmin"),
			cmd("stop", "Stop units", "sysadmin"),
			cmd("restart", "Restart units", "sysadmin"),
			cmd("status", "Show unit status", "sysadmin"),
			cmd("enable", "Enable units at boot", "sysadmin"),
			cmd("disable", "Disable units at boot", "sysadmin"),
			cmd("is-active", "Check whether units are active", "sysadmin"),
			cmd("is-enabled", "Check whether units are enabled", "sysadmin"),
			cmd("list-units", "List loaded units", "sysadmin"),
			cmd("list-unit-files", "List installed unit files", "sysadmin"),
			cmd("daemon-reload", "Reload the systemd manager", "sysadmin"),
			cmd("reload", "Reload unit configuration", "sysadmin"),
			cmd("mask", "Mask units", "sysadmin"),
			cmd("unmask", "Unmask units", "sysadmin"),
		},
	}
}

// tldrSpec builds the tldr spec.
func tldrSpec() *Spec {
	return &Spec{
		Name:        "tldr",
		Description: "Simplified manual pages",
		Icon:        "viewer",
		Options: []Option{
			optVal("Select the platform", "-p", "--platform"),
			optD("Update the local cache", "-u", "--update"),
		},
	}
}

// tmuxSpec builds the tmux spec.
func tmuxSpec() *Spec {
	return &Spec{
		Name:        "tmux",
		Description: "Terminal multiplexer",
		Icon:        "shell",
		Options: []Option{
			optGen("files", "Use a configuration file", "-f"),
			optD("Verbose logging", "-v"),
		},
		Subcommands: []*Spec{
			{
				Name: "new-session", Aliases: []string{"new"}, Description: "Create a session", Icon: "shell",
				Options: []Option{
					optVal("Name the session", "-s"),
					optD("Start detached", "-d"),
				},
			},
			{
				Name: "attach-session", Aliases: []string{"attach", "a"}, Description: "Attach to a session", Icon: "shell",
				Options: []Option{
					optVal("Target session", "-t"),
				},
			},
			{Name: "list-sessions", Aliases: []string{"ls"}, Description: "List sessions", Icon: "shell"},
			{
				Name: "kill-session", Description: "Kill a session", Icon: "shell",
				Options: []Option{
					optVal("Target session", "-t"),
				},
			},
			{
				Name: "split-window", Description: "Split a pane", Icon: "shell",
				Options: []Option{
					optD("Split horizontally", "-h"),
					optD("Split vertically", "-v"),
					optVal("Target pane", "-t"),
				},
			},
			{
				Name: "send-keys", Description: "Send keys to a pane", Icon: "shell",
				Options: []Option{
					optVal("Target pane", "-t"),
				},
			},
			{Name: "source-file", Description: "Run configuration commands from a file", Icon: "shell", Generator: "files"},
			cmd("new-window", "Create a window", "shell"),
			cmd("list-windows", "List windows", "shell"),
		},
	}
}

// topSpec builds the top spec.
func topSpec() *Spec {
	return &Spec{
		Name:        "top",
		Description: "Display and update process statistics",
		Icon:        "process",
		Options: []Option{
			optVal("Sort by a key", "-o"),
			optVal("Show only the given PIDs", "-p"),
			optVal("Show a user's processes", "-u"),
		},
	}
}

// unsetSpec builds the unset builtin spec.
func unsetSpec() *Spec {
	return &Spec{
		Name:        "unset",
		Description: "Unset shell variables",
		Icon:        "shell",
		Options: []Option{
			optD("Refer to shell variables", "-v"),
			optD("Refer to shell functions", "-f"),
		},
		Generator: "env-vars",
	}
}

// wgetSpec builds the wget spec.
func wgetSpec() *Spec {
	return &Spec{
		Name:        "wget",
		Description: "Download files from the web",
		Icon:        "network",
		Options: []Option{
			optGen("files", "Write output to file", "-O", "--output-document"),
			optD("Quiet mode", "-q", "--quiet"),
			optD("Continue a partial download", "-c", "--continue"),
			optD("Skip certificate validation", "--no-check-certificate"),
			optGen("dirs", "Directory prefix for downloads", "-P", "--directory-prefix"),
		},
	}
}

// whereSpec builds the where spec.
func whereSpec() *Spec {
	return &Spec{
		Name:        "where",
		Description: "Locate a command",
		Icon:        "search",
	}
}

// whereisSpec builds the whereis spec.
func whereisSpec() *Spec {
	return &Spec{
		Name:        "whereis",
		Description: "Locate binaries, sources, and manuals",
		Icon:        "search",
		Options: []Option{
			optD("Search only for binaries", "-b"),
			optD("Search only for manuals", "-m"),
			optD("Search only for sources", "-s"),
		},
	}
}

// whichSpec builds the which spec.
func whichSpec() *Spec {
	return &Spec{
		Name:        "which",
		Description: "Locate a command on PATH",
		Icon:        "search",
		Options: []Option{
			optD("Show all matches", "-a"),
		},
	}
}
