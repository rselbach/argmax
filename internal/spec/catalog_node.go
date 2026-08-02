package spec

// nodeCount is the PRD 18.2 category size (JavaScript, TypeScript,
// frontend, and Node.js).
const nodeCount = 82

func catalogNode() []*Spec {
	specs := []*Spec{
		bunSpec(),
		{
			Name:        "bunx",
			Description: "Execute a package binary with Bun",
			Icon:        "node",
			Options: []Option{
				optD("Use the latest version", "--bun"),
				optVal("Specify the package", "-p", "--package"),
			},
		},
		denoSpec(),
		nodeSpec(),
		npmSpec(),
		{
			Name:        "npx",
			Description: "Execute a package binary with npm",
			Icon:        "node",
			Options: []Option{
				optD("Install the package without prompting", "-y", "--yes"),
				optVal("Specify the package", "-p", "--package"),
			},
		},
		pnpmSpec(),
		{
			Name:        "pnpx",
			Description: "Execute a package binary with pnpm",
			Icon:        "node",
			Options: []Option{
				optVal("Specify the package", "-p", "--package"),
			},
		},
		yarnSpec(),
		{
			Name:        "astro",
			Description: "Astro site builder",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("dev", "Start the dev server", "node"),
				cmd("build", "Build the site", "node"),
				cmd("preview", "Preview the build", "node"),
			},
		},
		{
			Name:        "eslint",
			Description: "Lint JavaScript and TypeScript",
			Icon:        "node",
			Options: []Option{
				optD("Fix problems automatically", "--fix"),
				optVal("Lint specific extensions", "--ext"),
				optGen("files", "Use a specific config file", "-c", "--config"),
			},
			Generator: "files",
		},
		{
			Name:        "gatsby",
			Description: "Gatsby site framework",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("develop", "Start the dev server", "node"),
				cmd("build", "Build the site", "node"),
				cmd("serve", "Serve the build", "node"),
			},
		},
		{
			Name:        "jest",
			Description: "JavaScript test runner",
			Icon:        "node",
			Options: []Option{
				optD("Watch for changes", "--watch"),
				optD("Collect coverage", "--coverage"),
				optVal("Run tests matching a pattern", "-t"),
			},
		},
		{
			Name:        "nest",
			Description: "NestJS framework CLI",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("new", "Create a new project", "node"),
				cmd("generate", "Generate code schematics", "node"),
				cmd("start", "Start the application", "node"),
				cmd("build", "Build the application", "node"),
			},
		},
		{
			Name:        "next",
			Description: "Next.js framework CLI",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("dev", "Start the dev server", "node"),
				cmd("build", "Build the application", "node"),
				cmd("start", "Start the production server", "node"),
				cmd("lint", "Lint the project", "node"),
			},
		},
		{
			Name:        "ng",
			Description: "Angular CLI",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("new", "Create a new workspace", "node"),
				cmd("serve", "Start the dev server", "node"),
				cmd("build", "Build the application", "node"),
				cmd("generate", "Generate code schematics", "node"),
				cmd("test", "Run unit tests", "node"),
			},
		},
		{
			Name:        "nuxi",
			Description: "Nuxt CLI",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("dev", "Start the dev server", "node"),
				cmd("build", "Build the application", "node"),
				cmd("generate", "Generate the static site", "node"),
				cmd("preview", "Preview the build", "node"),
			},
		},
		{
			Name:        "playwright",
			Description: "Browser automation and testing",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("test", "Run tests", "node"),
				cmd("install", "Install browsers", "node"),
				cmd("show-report", "Show the test report", "node"),
				cmd("codegen", "Record actions as code", "node"),
			},
		},
		{
			Name:        "prettier",
			Description: "Opinionated code formatter",
			Icon:        "node",
			Options: []Option{
				optD("Write changes in place", "-w", "--write"),
				optD("Check formatting without writing", "-c", "--check"),
				optVal("Use a specific config file", "--config"),
			},
			Generator: "files",
		},
		{
			Name:        "remix",
			Description: "Remix framework CLI",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("dev", "Start the dev server", "node"),
				cmd("build", "Build the application", "node"),
			},
		},
		{
			Name:        "tsc",
			Description: "TypeScript compiler",
			Icon:        "node",
			Options: []Option{
				optD("Create a tsconfig.json", "--init"),
				optVal("Compile a specific project", "-p", "--project"),
				optD("Type-check without emitting", "--noEmit"),
				optD("Watch for changes", "-w", "--watch"),
			},
			Generator: "ext:ts,tsx",
		},
		{
			Name:        "turbo",
			Description: "Monorepo task runner",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("run", "Run a task", "node"),
				cmd("build", "Build packages", "node"),
				cmd("dev", "Run dev tasks", "node"),
				cmd("lint", "Lint packages", "node"),
				cmd("test", "Run tests", "node"),
			},
		},
		{
			Name:        "vite",
			Description: "Vite build tool",
			Icon:        "node",
			Subcommands: []*Spec{
				cmd("dev", "Start the dev server", "node"),
				cmd("build", "Build for production", "node"),
				cmd("preview", "Preview the build", "node"),
			},
		},
		{
			Name:        "webpack",
			Description: "JavaScript module bundler",
			Icon:        "node",
			Options: []Option{
				optGen("files", "Use a specific config file", "-c", "--config"),
				optVal("Set the build mode", "--mode"),
				optD("Watch for changes", "-w", "--watch"),
			},
		},
	}
	for _, e := range nodeSimple {
		specs = append(specs, cmd(e[0], e[1], e[2]))
	}
	return specs
}

// nodeSimple holds the single-level entries of PRD 18.2.
var nodeSimple = [][3]string{
	{"asar", "Electron archive tool", "node"},
	{"babel", "JavaScript compiler", "node"},
	{"blitz", "Blitz.js framework CLI", "node"},
	{"browser-sync", "Synchronized browser testing", "node"},
	{"build-storybook", "Build a Storybook", "node"},
	{"cordova", "Mobile hybrid app framework", "node"},
	{"create-completion-spec", "Scaffold a completion spec", "node"},
	{"create-next-app", "Scaffold a Next.js app", "node"},
	{"create-nx-workspace", "Scaffold an Nx workspace", "node"},
	{"create-react-app", "Scaffold a React app", "node"},
	{"create-react-native-app", "Scaffold a React Native app", "node"},
	{"create-redwood-app", "Scaffold a Redwood app", "node"},
	{"create-remix", "Scaffold a Remix app", "node"},
	{"create-t3-app", "Scaffold a T3 stack app", "node"},
	{"create-video", "Scaffold a Remotion video", "node"},
	{"create-vite", "Scaffold a Vite app", "node"},
	{"create-web3-frontend", "Scaffold a web3 frontend", "node"},
	{"dotenv", "Run commands with .env loaded", "node"},
	{"electron", "Cross-platform desktop apps", "node"},
	{"elm", "Elm language compiler", "build"},
	{"elm-format", "Format Elm source code", "build"},
	{"elm-json", "Manage elm.json files", "build"},
	{"elm-review", "Review Elm code", "build"},
	{"esbuild", "Fast JavaScript bundler", "node"},
	{"expo", "Expo React Native CLI", "node"},
	{"expo-cli", "Legacy Expo CLI", "node"},
	{"ganache-cli", "Local Ethereum blockchain", "node"},
	{"hardhat", "Ethereum development environment", "node"},
	{"ionic", "Ionic framework CLI", "node"},
	{"lerna", "Monorepo management tool", "node"},
	{"meteor", "Meteor JavaScript platform", "node"},
	{"ncu", "Upgrade package.json dependencies", "node"},
	{"nuxt", "Nuxt framework CLI", "node"},
	{"nx", "Nx monorepo CLI", "node"},
	{"oxlint", "Fast JavaScript linter", "node"},
	{"quasar", "Quasar framework CLI", "node"},
	{"react-native", "React Native CLI", "node"},
	{"redwood", "RedwoodJS framework CLI", "node"},
	{"remotion", "Programmatic video creation", "node"},
	{"rollup", "JavaScript module bundler", "node"},
	{"rome", "Unified web toolchain", "node"},
	{"rush", "Monorepo build orchestrator", "node"},
	{"sequelize", "Sequelize ORM CLI", "node"},
	{"serve", "Static file server", "node"},
	{"shadcn-ui", "shadcn/ui component CLI", "node"},
	{"start-storybook", "Start a Storybook", "node"},
	{"stencil", "Web component compiler", "node"},
	{"swagger-typescript-api", "Generate API clients from Swagger", "node"},
	{"swc", "Fast JavaScript/TypeScript compiler", "node"},
	{"truffle", "Ethereum development framework", "node"},
	{"ts-node", "Run TypeScript directly", "node"},
	{"tsx", "Run TypeScript with esbuild", "node"},
	{"typeorm", "TypeORM CLI", "node"},
	{"vr", "Deno task runner", "node"},
	{"vsce", "VS Code extension manager", "node"},
	{"vue", "Vue.js CLI", "node"},
	{"watchman", "File watching service", "node"},
	{"yalc", "Local package publishing", "node"},
}

// npmSpec builds the npm spec; `npm run` completes package.json scripts.
func npmSpec() *Spec {
	return &Spec{
		Name:        "npm",
		Description: "Node package manager",
		Icon:        "node",
		Subcommands: []*Spec{
			{Name: "run", Aliases: []string{"run-script"}, Description: "Run a package script", Icon: "node", Generator: "node-scripts"},
			cmd("test", "Run the test script", "node"),
			cmd("start", "Run the start script", "node"),
			{
				Name: "install", Aliases: []string{"i", "add"}, Description: "Install packages", Icon: "node",
				Options: []Option{
					optD("Save as a dev dependency", "-D", "--save-dev"),
					optD("Install globally", "-g", "--global"),
					optD("Save the exact version", "-E", "--save-exact"),
				},
			},
			{Name: "remove", Aliases: []string{"uninstall", "rm"}, Description: "Remove packages", Icon: "node"},
			{Name: "exec", Aliases: []string{"dlx"}, Description: "Run a command from a package", Icon: "node"},
			cmd("init", "Create a package.json", "node"),
			cmd("publish", "Publish the package", "node"),
			cmd("outdated", "Check for outdated packages", "node"),
			{Name: "update", Aliases: []string{"up"}, Description: "Update packages", Icon: "node"},
			cmd("audit", "Audit dependencies for vulnerabilities", "node"),
			{
				Name: "cache", Description: "Manage the package cache", Icon: "node",
				Subcommands: []*Spec{
					cmd("add", "Add a package to the cache", "node"),
					cmd("clean", "Clear the cache", "node"),
					cmd("verify", "Verify the cache", "node"),
					cmd("ls", "List cached packages", "node"),
				},
			},
		},
	}
}

// pnpmSpec builds the pnpm spec; `pnpm run` completes package.json scripts.
func pnpmSpec() *Spec {
	return &Spec{
		Name:        "pnpm",
		Description: "Fast, disk space efficient package manager",
		Icon:        "node",
		Subcommands: []*Spec{
			{Name: "run", Description: "Run a package script", Icon: "node", Generator: "node-scripts"},
			cmd("test", "Run the test script", "node"),
			cmd("start", "Run the start script", "node"),
			{
				Name: "add", Aliases: []string{"i", "install"}, Description: "Install packages", Icon: "node",
				Options: []Option{
					optD("Save as a dev dependency", "-D", "--save-dev"),
					optD("Install globally", "-g", "--global"),
					optD("Save the exact version", "-E", "--save-exact"),
				},
			},
			{Name: "remove", Aliases: []string{"uninstall", "rm"}, Description: "Remove packages", Icon: "node"},
			{Name: "exec", Aliases: []string{"dlx"}, Description: "Run a command from a package", Icon: "node"},
			cmd("init", "Create a package.json", "node"),
			cmd("publish", "Publish the package", "node"),
			cmd("outdated", "Check for outdated packages", "node"),
			cmd("update", "Update packages", "node"),
			cmd("audit", "Audit dependencies for vulnerabilities", "node"),
			cmd("cache", "Manage the package cache", "node"),
		},
	}
}

// yarnSpec builds the Yarn spec; `yarn run` completes package.json scripts.
func yarnSpec() *Spec {
	return &Spec{
		Name:        "yarn",
		Description: "Yarn package manager",
		Icon:        "node",
		Subcommands: []*Spec{
			{Name: "run", Description: "Run a package script", Icon: "node", Generator: "node-scripts"},
			cmd("test", "Run the test script", "node"),
			cmd("start", "Run the start script", "node"),
			{
				Name: "add", Description: "Install packages", Icon: "node",
				Options: []Option{
					optD("Save as a dev dependency", "-D", "--dev"),
					optD("Save the exact version", "-E", "--exact"),
				},
			},
			{Name: "remove", Aliases: []string{"uninstall"}, Description: "Remove packages", Icon: "node"},
			{Name: "exec", Aliases: []string{"dlx"}, Description: "Run a command from a package", Icon: "node"},
			cmd("init", "Create a package.json", "node"),
			cmd("publish", "Publish the package", "node"),
			cmd("outdated", "Check for outdated packages", "node"),
			{Name: "upgrade", Aliases: []string{"update"}, Description: "Update packages", Icon: "node"},
			cmd("audit", "Audit dependencies for vulnerabilities", "node"),
			{
				Name: "cache", Description: "Manage the package cache", Icon: "node",
				Subcommands: []*Spec{
					cmd("clean", "Clear the cache", "node"),
					cmd("list", "List cached packages", "node"),
				},
			},
		},
	}
}

// bunSpec builds the Bun spec; `bun run` completes package.json scripts.
func bunSpec() *Spec {
	return &Spec{
		Name:        "bun",
		Description: "Fast JavaScript runtime and toolkit",
		Icon:        "node",
		Subcommands: []*Spec{
			{Name: "run", Description: "Run a package script or file", Icon: "node", Generator: "node-scripts"},
			cmd("test", "Run tests", "node"),
			cmd("start", "Run the start script", "node"),
			{
				Name: "add", Aliases: []string{"i", "install"}, Description: "Install packages", Icon: "node",
				Options: []Option{
					optD("Save as a dev dependency", "-d", "--development"),
					optD("Install globally", "-g", "--global"),
					optD("Save the exact version", "-E", "--exact"),
				},
			},
			{Name: "remove", Aliases: []string{"rm", "uninstall"}, Description: "Remove packages", Icon: "node"},
			cmd("init", "Create a package.json", "node"),
			cmd("publish", "Publish the package", "node"),
			cmd("outdated", "Check for outdated packages", "node"),
			cmd("update", "Update packages", "node"),
			cmd("build", "Bundle the project", "node"),
			cmd("x", "Run a package binary", "node"),
		},
	}
}

// nodeSpec builds the node runtime spec.
func nodeSpec() *Spec {
	return &Spec{
		Name:        "node",
		Description: "Node.js JavaScript runtime",
		Icon:        "node",
		Options: []Option{
			optVal("Evaluate a script", "-e", "--eval"),
			optD("Show version", "-v", "--version"),
			optD("Watch for changes", "--watch"),
			optVal("Preload a module", "-r", "--require"),
		},
		Generator: "ext:js,mjs,cjs,ts",
	}
}

// denoSpec builds the Deno runtime spec.
func denoSpec() *Spec {
	return &Spec{
		Name:        "deno",
		Description: "Secure JavaScript and TypeScript runtime",
		Icon:        "node",
		Subcommands: []*Spec{
			{
				Name: "run", Description: "Run a program", Icon: "node",
				Options: []Option{
					optD("Allow all permissions", "-A", "--allow-all"),
					optD("Allow file system reads", "--allow-read"),
					optD("Allow network access", "--allow-net"),
					optD("Watch for changes", "--watch"),
				},
				Generator: "ext:js,ts",
			},
			{Name: "test", Description: "Run tests", Icon: "node", Generator: "ext:js,ts"},
			cmd("bench", "Run benchmarks", "node"),
			cmd("fmt", "Format source files", "node"),
			cmd("lint", "Lint source files", "node"),
			cmd("check", "Type-check modules", "node"),
			cmd("install", "Install a script as an executable", "node"),
			cmd("task", "Run a task", "node"),
		},
	}
}
