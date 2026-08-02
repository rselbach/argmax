package spec

// goDevCount is the PRD 18.5 category size (Go development).
const goDevCount = 3

func catalogGo() []*Spec {
	return []*Spec{
		goSpec(),
		cmd("goctl", "go-zero code generation tool", "go",
			cmd("api", "Generate api code", "go"),
			cmd("rpc", "Generate rpc code", "go"),
			cmd("model", "Generate model code", "go"),
			cmd("dockerfile", "Generate a Dockerfile", "go"),
			cmd("kube", "Generate Kubernetes manifests", "go"),
		),
		cmd("goreleaser", "Release Go projects", "go",
			cmd("release", "Publish a release", "go"),
			cmd("build", "Build the project", "go"),
			cmd("check", "Check the configuration", "go"),
			cmd("init", "Create a configuration file", "go"),
		),
	}
}

// goSpec builds the Go toolchain spec. `go run` completes Go source files;
// the other build-like subcommands complete directories (packages).
func goSpec() *Spec {
	return &Spec{
		Name:        "go",
		Description: "Go programming language toolchain",
		Icon:        "go",
		Options: []Option{
			optVal("Change to directory before running", "-C"),
		},
		Subcommands: []*Spec{
			{
				Name: "build", Description: "Compile packages", Icon: "go",
				Options: []Option{
					optGen("files", "Output file", "-o"),
					optD("Verbose output", "-v"),
					optD("Enable the race detector", "-race"),
					optVal("Build tags", "-tags"),
					optVal("Linker flags", "-ldflags"),
				},
				Generator: "dirs",
			},
			{
				Name: "run", Description: "Compile and run a program", Icon: "go",
				Options: []Option{
					optD("Enable the race detector", "-race"),
					optVal("Build tags", "-tags"),
				},
				Generator: "ext:go",
			},
			{
				Name: "test", Description: "Run package tests", Icon: "go",
				Options: []Option{
					optD("Verbose output", "-v"),
					optD("Enable the race detector", "-race"),
					optVal("Run the tests n times", "-count"),
					optVal("Run tests matching a pattern", "-run"),
					optVal("Run benchmarks matching a pattern", "-bench"),
					optD("Enable coverage analysis", "-cover"),
					optVal("Build tags", "-tags"),
				},
				Generator: "dirs",
			},
			{
				Name: "mod", Description: "Maintain the module", Icon: "go",
				Subcommands: []*Spec{
					cmd("tidy", "Add missing and remove unused modules", "go"),
					cmd("download", "Download modules to the cache", "go"),
					cmd("vendor", "Vendor the dependencies", "go"),
					cmd("init", "Initialize a new module", "go"),
					cmd("verify", "Verify module dependencies", "go"),
					cmd("graph", "Print the module requirement graph", "go"),
					cmd("why", "Explain why packages are needed", "go"),
					cmd("edit", "Edit the go.mod file", "go"),
				},
			},
			{
				Name: "get", Description: "Add dependencies", Icon: "go",
				Options: []Option{
					optD("Update modules providing packages", "-u"),
				},
			},
			cmd("install", "Compile and install packages", "go"),
			{Name: "vet", Description: "Report likely mistakes", Icon: "go", Generator: "dirs"},
			cmd("fmt", "Format packages", "go"),
			cmd("generate", "Run code generators", "go"),
			{
				Name: "env", Description: "Show the Go environment", Icon: "go",
				Options: []Option{
					optD("Change the default setting", "-w"),
					optD("Unset an environment setting", "-u"),
				},
			},
			{
				Name: "work", Description: "Manage workspaces", Icon: "go",
				Subcommands: []*Spec{
					cmd("init", "Initialize a workspace", "go"),
					cmd("use", "Add modules to the workspace", "go"),
					cmd("sync", "Sync the workspace", "go"),
					cmd("vendor", "Vendor the workspace", "go"),
				},
			},
			cmd("tool", "Run a go tool", "go"),
			{Name: "list", Description: "List packages or modules", Icon: "go", Generator: "dirs"},
			cmd("clean", "Remove object files and caches", "go"),
			cmd("doc", "Show package documentation", "go"),
		},
	}
}
