package spec

// ccCount is the PRD 18.7 category size (C/C++ compilers and build
// systems).
const ccCount = 16

func catalogC() []*Spec {
	return []*Spec{
		cmd("bazel", "Bazel build system", "build",
			cmd("build", "Build targets", "build"),
			cmd("test", "Build and run tests", "build"),
			cmd("run", "Build and run a target", "build"),
			cmd("query", "Query the dependency graph", "build"),
			cmd("clean", "Remove build outputs", "build"),
			cmd("info", "Show workspace information", "build"),
		),
		cCompilerSpec("c++", "Compile C++ source files"),
		cCompilerSpec("cc", "Compile C source files"),
		cCompilerSpec("clang", "Clang C compiler"),
		cCompilerSpec("clang++", "Clang C++ compiler"),
		{
			Name:        "cmake",
			Description: "Cross-platform build system generator",
			Icon:        "build",
			Options: []Option{
				optGen("dirs", "Source directory", "-S"),
				optGen("dirs", "Build directory", "-B"),
				optVal("Set a cache variable", "-D"),
				optD("Build a project", "--build"),
				optD("Install a project", "--install"),
				optVal("Use a specific generator", "-G"),
			},
		},
		cCompilerSpec("g++", "GNU C++ compiler"),
		cCompilerSpec("gcc", "GNU C compiler"),
		cmd("premake", "Generate project build files", "build"),
		{
			Name:        "swift",
			Description: "Swift language toolchain",
			Icon:        "c",
			Subcommands: []*Spec{
				cmd("build", "Build the package", "c"),
				cmd("test", "Run the tests", "c"),
				cmd("run", "Build and run an executable", "c"),
				{
					Name: "package", Description: "Manage packages", Icon: "c",
					Subcommands: []*Spec{
						cmd("init", "Create a package", "c"),
						cmd("update", "Update dependencies", "c"),
						cmd("resolve", "Resolve dependencies", "c"),
						cmd("describe", "Describe the package", "c"),
					},
				},
			},
		},
		{
			Name:        "typst",
			Description: "Markup-based typesetting system",
			Icon:        "text",
			Subcommands: []*Spec{
				cmd("compile", "Compile a document", "text"),
				cmd("watch", "Recompile on changes", "text"),
				cmd("init", "Initialize a project from a template", "text"),
				cmd("query", "Extract document values", "text"),
				cmd("fonts", "List available fonts", "text"),
			},
		},
		{
			Name:        "xcode-select",
			Description: "Manage the active developer directory",
			Icon:        "build",
			Options: []Option{
				optD("Print the developer directory", "-p", "--print-path"),
				optGen("dirs", "Set the developer directory", "-s", "--switch"),
				optD("Install command line tools", "--install"),
				optD("Reset to the default", "--reset"),
			},
		},
		{
			Name:        "xcodebuild",
			Description: "Build Xcode projects",
			Icon:        "build",
			Options: []Option{
				optVal("Build a specific scheme", "-scheme"),
				optVal("Build a specific target", "-target"),
				optVal("Use a build configuration", "-configuration"),
				optGen("files", "Build a project file", "-project"),
				optGen("files", "Build a workspace file", "-workspace"),
				optVal("Use a destination", "-destination"),
			},
		},
		cmd("xcodeproj", "Manage Xcode projects from Ruby", "build"),
		{
			Name:        "xcrun",
			Description: "Run developer tools",
			Icon:        "build",
			Options: []Option{
				optVal("Use a specific SDK", "-sdk"),
				optD("Find and print the tool path", "-f", "--find"),
				optD("Show verbose output", "-v", "--verbose"),
			},
		},
		{
			Name:        "zig",
			Description: "Zig language toolchain",
			Icon:        "c",
			Subcommands: []*Spec{
				cmd("build", "Build the project", "c"),
				cmd("run", "Build and run an executable", "c"),
				cmd("test", "Run tests", "c"),
				cmd("cc", "Use Zig as a C compiler", "c"),
				cmd("c++", "Use Zig as a C++ compiler", "c"),
				cmd("init", "Create a project", "c"),
			},
		},
	}
}

// cCompilerSpec builds the shared C/C++ compiler spec (gcc, g++, clang,
// clang++, cc, c++).
func cCompilerSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "c",
		Options: []Option{
			optGen("files", "Output file", "-o"),
			optD("Language standard (use as -std=...)", "-std"),
			optVal("Add an include search path", "-I"),
			optVal("Add a library search path", "-L"),
			optVal("Link a library", "-l"),
			optD("Optimization level (use as -O1/-O2/...)", "-O"),
			optD("Generate debug information", "-g"),
			optD("Enable common warnings", "-Wall"),
			optD("Compile without linking", "-c"),
		},
		Generator: "ext:c,cc,cpp,cxx,h,hpp",
	}
}
