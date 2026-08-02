package spec

// rustCount is the PRD 18.4 category size (Rust and modern CLI tools).
const rustCount = 11

func catalogRust() []*Spec {
	return []*Spec{
		cargoSpec(),
		cmd("dprint", "Pluggable code formatting platform", "build",
			cmd("fmt", "Format files", "build"),
			cmd("check", "Check formatting", "build"),
			cmd("init", "Create a configuration file", "build"),
		),
		cmd("pijul", "Patch-based version control", "vcs",
			cmd("init", "Create a repository", "vcs"),
			cmd("clone", "Clone a repository", "vcs"),
			cmd("record", "Record changes", "vcs"),
			cmd("diff", "Show changes", "vcs"),
			cmd("log", "Show the change log", "vcs"),
			cmd("channel", "Manage channels", "vcs"),
		),
		{
			Name:        "rustc",
			Description: "Rust compiler",
			Icon:        "rust",
			Options: []Option{
				optD("Optimize", "-O"),
				optVal("Set the edition", "--edition"),
				optGen("files", "Output file", "-o"),
				optVal("Set the crate type", "--crate-type"),
				optVal("Add a library search path", "-L"),
				optVal("Output types", "--emit"),
			},
			Generator: "files",
		},
		{
			Name:        "rustup",
			Description: "Rust toolchain installer",
			Icon:        "rust",
			Subcommands: []*Spec{
				cmd("update", "Update toolchains", "rust"),
				cmd("default", "Set the default toolchain", "rust"),
				{
					Name: "toolchain", Description: "Manage toolchains", Icon: "rust",
					Subcommands: []*Spec{
						cmd("install", "Install a toolchain", "rust"),
						cmd("list", "List toolchains", "rust"),
						cmd("uninstall", "Uninstall a toolchain", "rust"),
					},
				},
				{
					Name: "component", Description: "Manage components", Icon: "rust",
					Subcommands: []*Spec{
						cmd("add", "Add a component", "rust"),
						cmd("list", "List components", "rust"),
						cmd("remove", "Remove a component", "rust"),
					},
				},
				{
					Name: "target", Description: "Manage compilation targets", Icon: "rust",
					Subcommands: []*Spec{
						cmd("add", "Add a target", "rust"),
						cmd("list", "List targets", "rust"),
					},
				},
				cmd("show", "Show the active toolchain", "rust"),
				cmd("which", "Show a tool's location", "rust"),
			},
		},
		cmd("taplo", "TOML toolkit", "json",
			cmd("fmt", "Format TOML files", "json"),
			cmd("lint", "Lint TOML files", "json"),
		),
		cmd("tokei", "Count lines of code", "misc"),
		cmd("trunk", "Web application bundler for Rust/WASM", "rust",
			cmd("init", "Create a Trunk project", "rust"),
			cmd("build", "Build the project", "rust"),
			cmd("serve", "Serve with a dev server", "rust"),
			cmd("watch", "Watch and rebuild", "rust"),
			cmd("clean", "Clean build artifacts", "rust"),
		),
		{
			Name:        "wasm-bindgen",
			Description: "Generate JavaScript bindings for Rust/WASM",
			Icon:        "rust",
			Options: []Option{
				optVal("Set the target environment", "--target"),
				optVal("Set the output directory", "--out-dir"),
			},
		},
		cmd("wasm-pack", "Build and publish Rust/WASM packages", "rust",
			cmd("build", "Build the package", "rust"),
			cmd("test", "Run tests", "rust"),
			cmd("publish", "Publish the package", "rust"),
			cmd("init", "Deprecated: use build", "rust"),
		),
		{
			Name:        "zellij",
			Description: "Terminal multiplexer",
			Icon:        "shell",
			Subcommands: []*Spec{
				{Name: "attach", Aliases: []string{"a"}, Description: "Attach to a session", Icon: "shell"},
				{Name: "list-sessions", Aliases: []string{"ls"}, Description: "List sessions", Icon: "shell"},
				{Name: "kill-session", Aliases: []string{"k"}, Description: "Kill a session", Icon: "shell"},
				cmd("kill-all-sessions", "Kill all sessions", "shell"),
				cmd("setup", "Set up configuration", "shell"),
			},
		},
	}
}

// cargoSpec builds the Cargo spec; `cargo new` and `cargo init` complete
// directories.
func cargoSpec() *Spec {
	buildOpts := []Option{
		optD("Build in release mode", "--release"),
		optVal("Enable features", "--features"),
		optVal("Select the binary target", "--bin"),
		optVal("Select the package", "-p", "--package"),
	}
	return &Spec{
		Name:        "cargo",
		Description: "Rust package manager",
		Icon:        "rust",
		Options: []Option{
			optD("Show version", "-V", "--version"),
			optD("Quiet output", "-q", "--quiet"),
			optD("Verbose output", "-v", "--verbose"),
		},
		Subcommands: []*Spec{
			{Name: "build", Aliases: []string{"b"}, Description: "Compile the package", Icon: "rust", Options: buildOpts},
			{Name: "run", Aliases: []string{"r"}, Description: "Build and run a binary", Icon: "rust", Options: buildOpts},
			{Name: "test", Aliases: []string{"t"}, Description: "Run the tests", Icon: "rust", Options: buildOpts},
			{Name: "check", Aliases: []string{"c"}, Description: "Type-check without building", Icon: "rust", Options: buildOpts},
			{Name: "clippy", Description: "Lint with Clippy", Icon: "rust", Options: buildOpts},
			cmd("fmt", "Format the code", "rust"),
			{Name: "doc", Description: "Build the documentation", Icon: "rust", Options: []Option{optD("Open in the browser", "--open")}},
			{Name: "new", Description: "Create a new package", Icon: "rust", Generator: "dirs"},
			{Name: "init", Description: "Create a package in the current directory", Icon: "rust", Generator: "dirs"},
			{
				Name: "add", Description: "Add a dependency", Icon: "rust",
				Options: []Option{
					optD("Add as a dev dependency", "-D", "--dev"),
					optD("Add as a build dependency", "--build"),
					optD("Make the dependency optional", "--optional"),
					optVal("Enable features", "--features"),
				},
			},
			{Name: "remove", Aliases: []string{"rm"}, Description: "Remove a dependency", Icon: "rust"},
			{Name: "update", Description: "Update dependencies", Icon: "rust", Options: []Option{optD("Show what would be updated", "--dry-run")}},
			{Name: "publish", Description: "Publish the package", Icon: "rust", Options: []Option{optD("Verify without uploading", "--dry-run")}},
			{Name: "install", Description: "Install a binary crate", Icon: "rust", Options: []Option{optD("Force reinstall", "--force")}},
			{Name: "bench", Description: "Run the benchmarks", Icon: "rust", Options: buildOpts},
			{Name: "tree", Description: "Show the dependency tree", Icon: "rust"},
			{
				Name: "watch", Description: "Re-run a command on changes", Icon: "rust",
				Options: []Option{
					optVal("Command to run", "-x", "--exec"),
				},
			},
		},
	}
}
