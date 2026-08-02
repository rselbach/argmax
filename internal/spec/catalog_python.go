package spec

// pythonCount is the PRD 18.3 category size (Python and data science).
const pythonCount = 19

func catalogPython() []*Spec {
	return []*Spec{
		{
			Name:        "black",
			Description: "Uncompromising Python code formatter",
			Icon:        "python",
			Options: []Option{
				optD("Check formatting without writing", "--check"),
				optVal("Line length", "-l", "--line-length"),
				optD("Quiet mode", "-q", "--quiet"),
			},
			Generator: "ext:py",
		},
		condaSpec("conda", "Cross-platform package and environment manager"),
		{
			Name:        "django-admin",
			Description: "Django administration utility",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("startproject", "Create a new Django project", "python"),
				cmd("startapp", "Create a new Django app", "python"),
				cmd("migrate", "Apply database migrations", "python"),
				cmd("makemigrations", "Create database migrations", "python"),
				cmd("runserver", "Start the development server", "python"),
				cmd("createsuperuser", "Create an admin user", "python"),
				cmd("shell", "Start the interactive shell", "python"),
				cmd("test", "Run tests", "python"),
				cmd("check", "Check the project for problems", "python"),
				cmd("collectstatic", "Collect static files", "python"),
			},
		},
		cmd("googler", "Google from the command line", "search"),
		{
			Name:        "jupyter",
			Description: "Jupyter interactive computing",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("notebook", "Start the Jupyter notebook server", "python"),
				cmd("lab", "Start JupyterLab", "python"),
				cmd("run", "Execute a notebook", "python"),
				cmd("nbconvert", "Convert notebooks to other formats", "python"),
				cmd("list", "List running servers", "python"),
			},
		},
		condaSpec("mamba", "Fast conda-compatible package manager"),
		{
			Name:        "mypy",
			Description: "Static type checker for Python",
			Icon:        "python",
			Options: []Option{
				optD("Strict mode", "--strict"),
				optVal("Ignore errors in modules", "--ignore-missing-imports"),
				optVal("Python version to check against", "--python-version"),
			},
			Generator: "ext:py",
		},
		{
			Name:        "pipenv",
			Description: "Python virtualenv and package manager",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("install", "Install packages", "python"),
				cmd("uninstall", "Remove packages", "python"),
				cmd("run", "Run a command in the virtualenv", "python"),
				cmd("shell", "Spawn a shell in the virtualenv", "python"),
				cmd("sync", "Install all locked dependencies", "python"),
				cmd("lock", "Generate the lock file", "python"),
			},
		},
		{
			Name:        "pipx",
			Description: "Install and run Python applications in isolation",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("install", "Install a package", "python"),
				cmd("uninstall", "Remove a package", "python"),
				cmd("list", "List installed packages", "python"),
				cmd("run", "Run an application", "python"),
				cmd("inject", "Install into an existing environment", "python"),
				cmd("upgrade", "Upgrade packages", "python"),
			},
		},
		{
			Name:        "poetry",
			Description: "Python dependency management and packaging",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("add", "Add a dependency", "python"),
				cmd("remove", "Remove a dependency", "python"),
				cmd("install", "Install dependencies", "python"),
				cmd("run", "Run a command in the virtualenv", "python"),
				cmd("shell", "Spawn a shell in the virtualenv", "python"),
				cmd("init", "Create a pyproject.toml", "python"),
				cmd("update", "Update dependencies", "python"),
				cmd("build", "Build the package", "python"),
				cmd("publish", "Publish the package", "python"),
			},
		},
		{
			Name:        "pre-commit",
			Description: "Manage git pre-commit hooks",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("run", "Run hooks on files", "python"),
				cmd("install", "Install the hooks", "python"),
				cmd("autoupdate", "Update hook revisions", "python"),
				cmd("clean", "Remove cached environments", "python"),
				cmd("uninstall", "Uninstall the hooks", "python"),
			},
		},
		{
			Name:        "pyenv",
			Description: "Python version manager",
			Icon:        "python",
			Subcommands: []*Spec{
				cmd("install", "Install a Python version", "python"),
				cmd("uninstall", "Remove a Python version", "python"),
				cmd("global", "Set the global Python version", "python"),
				cmd("local", "Set the local Python version", "python"),
				cmd("versions", "List installed versions", "python"),
				cmd("version", "Show the current version", "python"),
				cmd("rehash", "Regenerate shims", "python"),
			},
		},
		{
			Name:        "pytest",
			Description: "Python testing framework",
			Icon:        "python",
			Options: []Option{
				optVal("Run tests matching an expression", "-k"),
				optD("Stop after the first failure", "-x", "--exitfirst"),
				optD("Verbose output", "-v", "--verbose"),
				optD("Disable output capturing", "-s"),
				optVal("Measure code coverage", "--cov"),
				optVal("Run tests with the given marks", "-m"),
			},
			Generator: "ext:py",
		},
		{
			Name:        "ruff",
			Description: "Extremely fast Python linter and formatter",
			Icon:        "python",
			Subcommands: []*Spec{
				{
					Name: "check", Description: "Lint Python files", Icon: "python",
					Options: []Option{
						optD("Fix problems automatically", "--fix"),
						optD("Watch for changes", "--watch"),
					},
					Generator: "ext:py",
				},
				{Name: "format", Description: "Format Python files", Icon: "python", Generator: "ext:py"},
			},
			Generator: "ext:py",
		},
		{
			Name:        "sqlfluff",
			Description: "SQL linter and formatter",
			Icon:        "database",
			Subcommands: []*Spec{
				cmd("lint", "Lint SQL files", "database"),
				cmd("fix", "Fix SQL files", "database"),
				cmd("format", "Format SQL files", "database"),
				cmd("parse", "Parse SQL files", "database"),
			},
		},
		{
			Name:        "sqlmesh",
			Description: "SQL transformation framework",
			Icon:        "database",
			Subcommands: []*Spec{
				cmd("plan", "Plan changes", "database"),
				cmd("run", "Run the project", "database"),
				cmd("test", "Run unit tests", "database"),
				cmd("init", "Create a project", "database"),
			},
		},
		{
			Name:        "streamlit",
			Description: "Data app framework",
			Icon:        "python",
			Subcommands: []*Spec{
				{Name: "run", Description: "Run an app", Icon: "python", Generator: "ext:py"},
				cmd("hello", "Run the demo app", "python"),
				cmd("docs", "Open the documentation", "python"),
			},
		},
		uvSpec(),
		{
			Name:        "youtube-dl",
			Description: "Download videos from the web",
			Icon:        "network",
			Options: []Option{
				optD("Extract audio", "-x", "--extract-audio"),
				optVal("Select a format", "-f", "--format"),
				optVal("Output file template", "-o", "--output"),
			},
		},
	}
}

// condaSpec builds the conda/mamba spec (mamba mirrors conda).
func condaSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "python",
		Subcommands: []*Spec{
			{
				Name: "create", Description: "Create an environment", Icon: "python",
				Options: []Option{
					optVal("Environment name", "-n", "--name"),
					optVal("Environment prefix path", "-p", "--prefix"),
				},
			},
			cmd("install", "Install packages", "python"),
			cmd("remove", "Remove packages", "python"),
			cmd("activate", "Activate an environment", "python"),
			cmd("deactivate", "Deactivate the environment", "python"),
			{
				Name: "env", Description: "Manage environments", Icon: "python",
				Subcommands: []*Spec{
					cmd("create", "Create an environment from a file", "python"),
					cmd("list", "List environments", "python"),
					cmd("remove", "Remove an environment", "python"),
					cmd("export", "Export an environment", "python"),
				},
			},
			cmd("list", "List installed packages", "python"),
			cmd("search", "Search for packages", "python"),
			cmd("update", "Update packages", "python"),
		},
	}
}

// uvSpec builds the uv spec; `uv pip uninstall` completes installed pip
// packages.
func uvSpec() *Spec {
	return &Spec{
		Name:        "uv",
		Description: "Fast Python package manager",
		Icon:        "python",
		Subcommands: []*Spec{
			{
				Name: "pip", Description: "Manage packages pip-style", Icon: "python",
				Subcommands: []*Spec{
					cmd("install", "Install packages", "python"),
					{Name: "uninstall", Description: "Remove packages", Icon: "python", Generator: "pip-packages"},
					cmd("list", "List installed packages", "python"),
					cmd("show", "Show package details", "python"),
					cmd("freeze", "List installed packages as requirements", "python"),
					cmd("download", "Download packages", "python"),
					cmd("compile", "Compile a requirements file", "python"),
					cmd("sync", "Sync the environment with a requirements file", "python"),
				},
			},
			cmd("venv", "Create a virtual environment", "python"),
			cmd("run", "Run a command in the project environment", "python"),
			cmd("add", "Add a dependency", "python"),
			cmd("remove", "Remove a dependency", "python"),
			cmd("sync", "Sync the project environment", "python"),
			cmd("init", "Create a project", "python"),
			cmd("lock", "Update the lock file", "python"),
		},
	}
}
