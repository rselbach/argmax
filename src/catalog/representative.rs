use crate::completion::{
    CommandSpec, FilesystemGenerator, GeneratorKind, GeneratorSpec, GeneratorTarget,
    InsertionBehavior, OptionSpec,
};

pub(super) fn spec(name: &str, description: &str) -> Option<CommandSpec> {
    Some(match name {
        "docker" => docker(description),
        "npm" => npm(description),
        "pnpm" | "yarn" | "bun" => node_package_manager(name, description),
        "pip" => pip(description),
        "cargo" => cargo(description),
        "go" => go(description),
        "mvn" => mvn(description),
        "cmake" => cmake(description),
        "git" => git(description),
        "apt" => apt(description),
        "tar" => tar(description),
        "vim" => vim(description),
        "jq" => jq(description),
        "just" => just(description),
        "make" => make(description),
        "ssh" => ssh(description),
        "kill" => kill(description),
        "systemctl" => systemctl(description),
        "zoxide" => zoxide(description),
        "printenv" => printenv(description),
        "fd" => fd(description),
        _ => return None,
    })
}

pub(super) fn supplemental_specs() -> [CommandSpec; 3] {
    [
        zoxide("query the zoxide directory database"),
        printenv("print selected environment variables"),
        fd("find filesystem entries"),
    ]
}

fn docker(description: &str) -> CommandSpec {
    CommandSpec::new("docker", description)
        .with_option(global_value_option("--context", "select a Docker context"))
        .with_option(global_value_option(
            "--config",
            "select the client configuration directory",
        ))
        .with_option(terminal_option("--version", "print version information"))
        .with_subcommand(
            CommandSpec::new("build", "build an image from a Dockerfile")
                .with_option(
                    value_option("-t", "name and optionally tag the image").with_alias("--tag"),
                )
                .with_option(value_option("-f", "select the Dockerfile").with_alias("--file"))
                .with_option(OptionSpec::new(
                    "--pull",
                    "always attempt to pull newer images",
                )),
        )
        .with_subcommand(
            CommandSpec::new("compose", "manage multi-container applications")
                .with_option(
                    global_value_option("-f", "select a Compose file").with_alias("--file"),
                )
                .with_option(
                    global_value_option("-p", "set the Compose project name")
                        .with_alias("--project-name"),
                )
                .with_subcommand(
                    CommandSpec::new("up", "create and start services")
                        .with_option(
                            OptionSpec::new("-d", "run services in the background")
                                .with_alias("--detach"),
                        )
                        .with_option(OptionSpec::new("--build", "build images before starting")),
                )
                .with_subcommand(
                    CommandSpec::new("down", "stop and remove services").with_option(
                        OptionSpec::new("-v", "remove named volumes").with_alias("--volumes"),
                    ),
                )
                .with_subcommand(no_positionals("ps", "list service containers"))
                .with_subcommand(CommandSpec::new("logs", "view service output")),
        )
        .with_subcommand(
            CommandSpec::new("run", "run a command in a new container")
                .with_option(OptionSpec::new(
                    "--rm",
                    "remove the container after it exits",
                ))
                .with_option(OptionSpec::new("-d", "run in the background").with_alias("--detach"))
                .with_option(value_option("--name", "assign a container name"))
                .with_generator(positional_generator(GeneratorKind::DockerImages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("exec", "run a command in a running container")
                .with_option(
                    OptionSpec::new("-i", "keep standard input open").with_alias("--interactive"),
                )
                .with_option(
                    OptionSpec::new("-t", "allocate a pseudo-terminal").with_alias("--tty"),
                )
                .with_generator(positional_generator(GeneratorKind::DockerContainers, 0)),
        )
        .with_subcommand(no_positionals("ps", "list containers"))
        .with_subcommand(no_positionals("images", "list images"))
        .with_subcommand(no_positionals("version", "show client and server versions"))
}

fn npm(description: &str) -> CommandSpec {
    CommandSpec::new("npm", description)
        .with_option(global_value_option(
            "--prefix",
            "run from another directory",
        ))
        .with_option(terminal_option("--version", "print the npm version").with_alias("-v"))
        .with_subcommand(no_positionals("ci", "install exactly from the lockfile"))
        .with_subcommand(
            CommandSpec::new("install", "install packages")
                .with_alias("i")
                .with_option(
                    OptionSpec::new("-D", "save as a development dependency")
                        .with_alias("--save-dev"),
                )
                .with_option(OptionSpec::new("-g", "install globally").with_alias("--global"))
                .with_option(OptionSpec::new(
                    "--ignore-scripts",
                    "skip package lifecycle scripts",
                )),
        )
        .with_subcommand(
            CommandSpec::new("uninstall", "remove packages")
                .with_alias("rm")
                .with_alias("remove")
                .with_option(
                    OptionSpec::new("-g", "remove global packages").with_alias("--global"),
                ),
        )
        .with_subcommand(
            CommandSpec::new("run", "run a package script")
                .with_option(OptionSpec::new(
                    "--if-present",
                    "succeed when the script is absent",
                ))
                .with_generator(positional_generator(GeneratorKind::PackageScripts, 0)),
        )
        .with_subcommand(CommandSpec::new("exec", "run a package-provided command"))
        .with_subcommand(CommandSpec::new("init", "create a package manifest"))
        .with_subcommand(CommandSpec::new("publish", "publish a package"))
        .with_subcommand(no_positionals("outdated", "list outdated dependencies"))
        .with_subcommand(no_positionals("test", "run the package test script"))
}

fn node_package_manager(name: &str, description: &str) -> CommandSpec {
    CommandSpec::new(name, description)
        .with_option(terminal_option("--version", "print version information"))
        .with_subcommand(
            CommandSpec::new("run", "run a package script")
                .with_generator(positional_generator(GeneratorKind::PackageScripts, 0)),
        )
}

fn pip(description: &str) -> CommandSpec {
    CommandSpec::new("pip", description)
        .with_option(
            OptionSpec::new("--isolated", "ignore environment and user configuration").global(true),
        )
        .with_option(global_value_option(
            "--python",
            "run pip for another interpreter",
        ))
        .with_option(terminal_option("--version", "print the pip version"))
        .with_subcommand(
            CommandSpec::new("install", "install packages")
                .with_option(
                    value_option("-r", "install from a requirements file")
                        .with_alias("--requirement"),
                )
                .with_option(
                    value_option("-e", "install a project in editable mode")
                        .with_alias("--editable"),
                )
                .with_option(OptionSpec::new("-U", "upgrade packages").with_alias("--upgrade"))
                .with_generator(positional_generator(GeneratorKind::Packages, 0))
                .with_generator(option_files("-r", &["txt", "in"])),
        )
        .with_subcommand(
            CommandSpec::new("uninstall", "remove installed packages")
                .with_option(OptionSpec::new("-y", "confirm all removals").with_alias("--yes"))
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("list", "list installed packages")
                .with_option(
                    OptionSpec::new("-o", "show outdated packages").with_alias("--outdated"),
                )
                .with_option(OptionSpec::new(
                    "--not-required",
                    "show packages not required by others",
                )),
        )
        .with_subcommand(
            CommandSpec::new("show", "show package information")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(no_positionals(
            "freeze",
            "print installed packages in requirements format",
        ))
        .with_subcommand(CommandSpec::new(
            "download",
            "download packages without installing",
        ))
        .with_subcommand(CommandSpec::new("wheel", "build wheels from requirements"))
        .with_subcommand(no_positionals(
            "check",
            "verify installed package dependencies",
        ))
        .with_subcommand(
            CommandSpec::new("cache", "inspect and manage pip's cache")
                .with_subcommand(no_positionals("dir", "show the cache directory"))
                .with_subcommand(no_positionals("info", "show cache information"))
                .with_subcommand(CommandSpec::new("list", "list cached artifacts"))
                .with_subcommand(CommandSpec::new(
                    "remove",
                    "remove matching cached artifacts",
                ))
                .with_subcommand(no_positionals("purge", "remove all cached artifacts")),
        )
}

fn cargo(description: &str) -> CommandSpec {
    CommandSpec::new("cargo", description)
        .with_option(global_value_option("--color", "control colored output"))
        .with_option(
            OptionSpec::new("--locked", "require Cargo.lock to remain unchanged").global(true),
        )
        .with_option(OptionSpec::new("--offline", "avoid network access").global(true))
        .with_option(terminal_option("--version", "print the Cargo version").with_alias("-V"))
        .with_subcommand(
            cargo_build_command("build", "b", "compile the current package")
                .with_option(OptionSpec::new("--release", "build optimized artifacts")),
        )
        .with_subcommand(cargo_build_command(
            "check",
            "c",
            "check the current package without producing binaries",
        ))
        .with_subcommand(
            cargo_build_command("run", "r", "run a package binary")
                .with_option(value_option("--bin", "select a binary target")),
        )
        .with_subcommand(
            cargo_build_command("test", "t", "run package tests").with_option(OptionSpec::new(
                "--no-run",
                "compile tests without running them",
            )),
        )
        .with_subcommand(
            CommandSpec::new("add", "add dependencies")
                .with_option(OptionSpec::new("--dev", "add a development dependency"))
                .with_option(value_option("--features", "enable dependency features"))
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("remove", "remove dependencies")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(no_positionals(
            "update",
            "update dependencies in Cargo.lock",
        ))
        .with_subcommand(no_positionals("fmt", "format Rust source"))
        .with_subcommand(no_positionals("clippy", "run Rust lints"))
}

fn cargo_build_command(name: &str, alias: &str, description: &str) -> CommandSpec {
    CommandSpec::new(name, description)
        .with_alias(alias)
        .with_option(value_option("--manifest-path", "select a Cargo.toml file"))
        .with_option(value_option("-p", "select a package").with_alias("--package"))
        .with_option(value_option("--target", "select a compilation target"))
        .with_generator(option_generator(GeneratorKind::Packages, "--package"))
        .with_generator(option_files("--manifest-path", &["toml"]))
}

fn go(description: &str) -> CommandSpec {
    CommandSpec::new("go", description)
        .with_option(global_value_option("-C", "change directory before running"))
        .with_subcommand(
            CommandSpec::new("build", "compile packages and dependencies")
                .with_option(value_option("-o", "write output to a file"))
                .with_option(OptionSpec::new("-race", "enable the race detector")),
        )
        .with_subcommand(
            CommandSpec::new("test", "test packages")
                .with_option(value_option("-run", "run tests matching a pattern"))
                .with_option(OptionSpec::new("-v", "print verbose test output"))
                .with_option(OptionSpec::new("-race", "enable the race detector")),
        )
        .with_subcommand(CommandSpec::new("run", "compile and run a Go program"))
        .with_subcommand(
            CommandSpec::new("get", "resolve and add dependencies")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("install", "compile and install packages")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(CommandSpec::new("fmt", "format package source"))
        .with_subcommand(CommandSpec::new(
            "vet",
            "report suspicious package constructs",
        ))
        .with_subcommand(CommandSpec::new(
            "env",
            "print or change Go environment settings",
        ))
        .with_subcommand(no_positionals("version", "print the Go version"))
        .with_subcommand(
            CommandSpec::new("mod", "manage Go modules")
                .with_subcommand(
                    CommandSpec::new("init", "create a go.mod file").with_max_positionals(1),
                )
                .with_subcommand(no_positionals(
                    "tidy",
                    "add missing and remove unused modules",
                ))
                .with_subcommand(no_positionals(
                    "download",
                    "download modules to the local cache",
                ))
                .with_subcommand(no_positionals("verify", "verify cached module content"))
                .with_subcommand(no_positionals("vendor", "create a vendor directory"))
                .with_subcommand(no_positionals("graph", "print the module dependency graph")),
        )
}

fn mvn(description: &str) -> CommandSpec {
    CommandSpec::new("mvn", description)
        .with_option(global_value_option("-f", "select an alternate POM").with_alias("--file"))
        .with_option(
            global_value_option("-P", "activate build profiles").with_alias("--activate-profiles"),
        )
        .with_option(
            OptionSpec::new("-q", "show only errors")
                .with_alias("--quiet")
                .global(true),
        )
        .with_option(
            OptionSpec::new("-o", "work offline")
                .with_alias("--offline")
                .global(true),
        )
        .with_option(
            terminal_option("-v", "print Maven version information").with_alias("--version"),
        )
        .with_generator(option_files("-f", &["xml"]))
        .with_subcommand(CommandSpec::new("clean", "remove build output"))
        .with_subcommand(CommandSpec::new("validate", "validate project structure"))
        .with_subcommand(CommandSpec::new("compile", "compile project sources"))
        .with_subcommand(CommandSpec::new("test", "run project tests"))
        .with_subcommand(CommandSpec::new(
            "package",
            "create the distributable package",
        ))
        .with_subcommand(CommandSpec::new(
            "verify",
            "run project verification checks",
        ))
        .with_subcommand(CommandSpec::new("install", "install artifacts locally"))
        .with_subcommand(CommandSpec::new("deploy", "publish artifacts remotely"))
        .with_subcommand(CommandSpec::new("site", "generate project documentation"))
}

fn cmake(description: &str) -> CommandSpec {
    CommandSpec::new("cmake", description)
        .with_option(value_option("-S", "select the source directory"))
        .with_option(value_option("-B", "select the build directory"))
        .with_option(value_option("-G", "select a build-system generator"))
        .with_option(value_option("-D", "define a cache variable").repeatable(true))
        .with_option(value_option("-U", "remove matching cache variables").repeatable(true))
        .with_option(value_option("-C", "preload a cache script"))
        .with_option(value_option("--toolchain", "select a toolchain file"))
        .with_option(value_option("--preset", "select a configure preset"))
        .with_option(value_option("--build", "build a configured tree"))
        .with_option(value_option("--install", "install a configured project"))
        .with_option(OptionSpec::new("--fresh", "configure with a fresh cache"))
        .with_option(terminal_option(
            "--version",
            "print CMake version information",
        ))
        .with_option(terminal_option("--help", "print CMake usage information"))
        .with_generator(option_directories("-S"))
        .with_generator(option_directories("-B"))
        .with_generator(option_directories("--build"))
        .with_generator(option_directories("--install"))
        .with_generator(option_files("-C", &["cmake"]))
        .with_generator(option_files("--toolchain", &["cmake"]))
}

fn git(description: &str) -> CommandSpec {
    CommandSpec::new("git", description)
        .with_option(global_value_option(
            "-C",
            "run as if started in another directory",
        ))
        .with_option(OptionSpec::new("--no-pager", "disable paging").global(true))
        .with_option(global_value_option(
            "--git-dir",
            "select the repository directory",
        ))
        .with_option(global_value_option(
            "--work-tree",
            "select the working tree",
        ))
        .with_option(terminal_option(
            "--version",
            "print Git version information",
        ))
        .with_subcommand(
            CommandSpec::new("add", "add file contents to the index")
                .with_option(OptionSpec::new("-A", "stage all changes").with_alias("--all"))
                .with_option(
                    OptionSpec::new("-p", "interactively select changes").with_alias("--patch"),
                )
                .with_generator(positional_generator(GeneratorKind::GitFiles, 0)),
        )
        .with_subcommand(
            CommandSpec::new("commit", "record changes to the repository")
                .with_option(
                    value_option("-m", "use the supplied commit message").with_alias("--message"),
                )
                .with_option(
                    OptionSpec::new("-a", "stage tracked file changes").with_alias("--all"),
                )
                .with_option(OptionSpec::new("--amend", "replace the previous commit")),
        )
        .with_subcommand(
            CommandSpec::new("branch", "list or manage branches")
                .with_option(OptionSpec::new("-d", "delete a merged branch").with_alias("--delete"))
                .with_option(OptionSpec::new("-m", "move or rename a branch").with_alias("--move"))
                .with_generator(positional_generator(GeneratorKind::GitBranches, 0)),
        )
        .with_subcommand(
            CommandSpec::new("checkout", "switch branches or restore files")
                .with_option(value_option("-b", "create and switch to a branch"))
                .with_generator(positional_generator(GeneratorKind::GitBranches, 0))
                .with_generator(positional_generator(GeneratorKind::GitCommits, 0)),
        )
        .with_subcommand(
            CommandSpec::new("switch", "switch branches")
                .with_option(
                    value_option("-c", "create and switch to a branch").with_alias("--create"),
                )
                .with_generator(positional_generator(GeneratorKind::GitBranches, 0)),
        )
        .with_subcommand(
            CommandSpec::new("log", "show commit logs")
                .with_option(
                    value_option("-n", "limit the number of commits").with_alias("--max-count"),
                )
                .with_option(OptionSpec::new("--oneline", "show one line per commit"))
                .with_generator(positional_generator(GeneratorKind::GitCommits, 0)),
        )
        .with_subcommand(
            CommandSpec::new("tag", "create, list, or delete tags")
                .with_option(OptionSpec::new("-d", "delete tags").with_alias("--delete"))
                .with_generator(positional_generator(GeneratorKind::GitTags, 0)),
        )
        .with_subcommand(
            no_positionals("status", "show the working tree status").with_option(
                OptionSpec::new("-s", "show short-format status").with_alias("--short"),
            ),
        )
        .with_subcommand(git_remote())
        .with_subcommand(git_stash())
}

fn git_remote() -> CommandSpec {
    CommandSpec::new("remote", "manage tracked repositories")
        .with_subcommand(CommandSpec::new("add", "add a remote").with_max_positionals(2))
        .with_subcommand(
            CommandSpec::new("get-url", "read remote URLs")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitRemotes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("remove", "remove a remote")
                .with_alias("rm")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitRemotes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("rename", "rename a remote")
                .with_max_positionals(2)
                .with_generator(positional_generator(GeneratorKind::GitRemotes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("set-url", "change remote URLs")
                .with_generator(positional_generator(GeneratorKind::GitRemotes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("show", "inspect a remote")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitRemotes, 0)),
        )
}

fn git_stash() -> CommandSpec {
    CommandSpec::new("stash", "stash working tree changes")
        .with_subcommand(CommandSpec::new("push", "save local modifications"))
        .with_subcommand(no_positionals("list", "list stashed changes"))
        .with_subcommand(
            CommandSpec::new("show", "inspect a stashed change")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitStashes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("apply", "apply a stashed change")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitStashes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("drop", "remove a stashed change")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitStashes, 0)),
        )
        .with_subcommand(
            CommandSpec::new("pop", "apply and remove a stashed change")
                .with_max_positionals(1)
                .with_generator(positional_generator(GeneratorKind::GitStashes, 0)),
        )
}

fn apt(description: &str) -> CommandSpec {
    CommandSpec::new("apt", description)
        .with_option(
            OptionSpec::new("-y", "assume yes for prompts")
                .with_alias("--yes")
                .global(true),
        )
        .with_option(
            OptionSpec::new("-q", "reduce command output")
                .with_alias("--quiet")
                .repeatable(true)
                .global(true),
        )
        .with_option(terminal_option(
            "--version",
            "print apt version information",
        ))
        .with_subcommand(no_positionals("update", "refresh package indexes"))
        .with_subcommand(no_positionals("upgrade", "upgrade installed packages"))
        .with_subcommand(
            no_positionals("full-upgrade", "upgrade packages with dependency changes")
                .with_alias("dist-upgrade"),
        )
        .with_subcommand(
            CommandSpec::new("install", "install packages")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("remove", "remove packages")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(
            CommandSpec::new("purge", "remove packages and configuration")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(CommandSpec::new(
            "autoremove",
            "remove unneeded dependencies",
        ))
        .with_subcommand(CommandSpec::new("search", "search package descriptions"))
        .with_subcommand(
            CommandSpec::new("show", "show package details")
                .with_generator(positional_generator(GeneratorKind::Packages, 0)),
        )
        .with_subcommand(CommandSpec::new("list", "list packages"))
        .with_subcommand(no_positionals(
            "edit-sources",
            "edit software source configuration",
        ))
}

fn tar(description: &str) -> CommandSpec {
    CommandSpec::new("tar", description)
        .with_option(OptionSpec::new("-c", "create an archive").with_alias("--create"))
        .with_option(OptionSpec::new("-x", "extract an archive").with_alias("--extract"))
        .with_option(OptionSpec::new("-t", "list archive contents").with_alias("--list"))
        .with_option(value_option("-f", "select the archive file").with_alias("--file"))
        .with_option(value_option("-C", "change directory").with_alias("--directory"))
        .with_option(OptionSpec::new("-z", "filter through gzip").with_alias("--gzip"))
        .with_option(OptionSpec::new("-j", "filter through bzip2").with_alias("--bzip2"))
        .with_option(OptionSpec::new("-J", "filter through xz").with_alias("--xz"))
        .with_option(OptionSpec::new("-v", "list processed files").with_alias("--verbose"))
        .with_option(value_option(
            "--strip-components",
            "strip leading path components",
        ))
        .with_option(value_option("--exclude", "exclude matching members").repeatable(true))
        .with_option(terminal_option(
            "--version",
            "print tar version information",
        ))
        .with_generator(option_files("-f", &["tar", "tgz", "gz", "bz2", "xz"]))
        .with_generator(option_directories("-C"))
        .with_generator(positional_files(0))
}

fn vim(description: &str) -> CommandSpec {
    CommandSpec::new("vim", description)
        .with_option(OptionSpec::new("-R", "open files read-only"))
        .with_option(OptionSpec::new("-d", "open files in diff mode"))
        .with_option(OptionSpec::new("-p", "open files in tab pages"))
        .with_option(OptionSpec::new("-o", "open files in horizontal windows"))
        .with_option(OptionSpec::new("-O", "open files in vertical windows"))
        .with_option(value_option("-u", "use another vimrc file"))
        .with_option(value_option("-S", "source a session file"))
        .with_option(value_option("-c", "run an Ex command after loading").repeatable(true))
        .with_option(value_option("--cmd", "run an Ex command before loading").repeatable(true))
        .with_option(OptionSpec::new("--clean", "start with clean defaults"))
        .with_option(terminal_option(
            "--version",
            "print Vim version information",
        ))
        .with_option(terminal_option("--help", "print Vim usage information"))
        .with_generator(positional_files(0))
        .with_generator(option_files("-u", &["vim", "vimrc"]))
        .with_generator(option_files("-S", &["vim"]))
}

fn jq(description: &str) -> CommandSpec {
    CommandSpec::new("jq", description)
        .with_option(OptionSpec::new("-r", "write raw strings").with_alias("--raw-output"))
        .with_option(OptionSpec::new("-c", "write compact JSON").with_alias("--compact-output"))
        .with_option(
            OptionSpec::new("-n", "use null instead of reading input").with_alias("--null-input"),
        )
        .with_option(OptionSpec::new("-s", "read inputs into an array").with_alias("--slurp"))
        .with_option(
            OptionSpec::new("-e", "set exit status from the result").with_alias("--exit-status"),
        )
        .with_option(value_option("-f", "read the filter from a file").with_alias("--from-file"))
        .with_option(value_option("-L", "prepend a module search directory").repeatable(true))
        .with_option(value_option("--indent", "set indentation width"))
        .with_option(terminal_option("--version", "print jq version information"))
        .with_option(terminal_option("--help", "print jq usage information"))
        .with_generator(option_files("-f", &["jq"]))
        .with_generator(positional_files(1))
}

fn just(description: &str) -> CommandSpec {
    CommandSpec::new("just", description)
        .with_option(value_option("-f", "select a justfile").with_alias("--justfile"))
        .with_option(
            value_option("-d", "select the working directory").with_alias("--working-directory"),
        )
        .with_option(OptionSpec::new("-l", "list available recipes").with_alias("--list"))
        .with_option(
            OptionSpec::new("-n", "print recipes without running them").with_alias("--dry-run"),
        )
        .with_option(OptionSpec::new("--choose", "select a recipe interactively"))
        .with_option(OptionSpec::new("--dump", "print the parsed justfile"))
        .with_option(OptionSpec::new(
            "--evaluate",
            "evaluate and print variables",
        ))
        .with_option(OptionSpec::new(
            "--fmt",
            "format and overwrite the justfile",
        ))
        .with_option(OptionSpec::new("--init", "create a new justfile"))
        .with_option(value_option("--show", "show a recipe definition"))
        .with_option(OptionSpec::new("--summary", "list recipe names"))
        .with_option(OptionSpec::new("--unsorted", "preserve declaration order"))
        .with_option(OptionSpec::new("--unstable", "enable unstable features"))
        .with_option(
            terminal_option("--version", "print just version information").with_alias("-V"),
        )
        .with_generator(positional_generator(GeneratorKind::JustRecipes, 0))
        .with_generator(option_files("-f", &["just", "justfile"]))
        .with_generator(option_directories("-d"))
}

fn make(description: &str) -> CommandSpec {
    CommandSpec::new("make", description)
        .with_option(value_option("-f", "read another makefile").with_alias("--file"))
        .with_generator(positional_generator(GeneratorKind::MakeTargets, 0))
}

fn ssh(description: &str) -> CommandSpec {
    CommandSpec::new("ssh", description)
        .with_option(value_option("-p", "connect to a remote port"))
        .with_generator(positional_generator(GeneratorKind::SshHosts, 0))
}

fn kill(description: &str) -> CommandSpec {
    CommandSpec::new("kill", description)
        .with_option(value_option("-s", "select the signal to send"))
        .with_generator(positional_generator(GeneratorKind::Processes, 0))
}

fn zoxide(description: &str) -> CommandSpec {
    CommandSpec::new("zoxide", description)
        .with_subcommand(
            CommandSpec::new("query", "search the directory database")
                .with_generator(positional_generator(GeneratorKind::ZoxideDirectories, 0)),
        )
        .with_subcommand(CommandSpec::new("add", "add a directory to the database"))
        .with_subcommand(CommandSpec::new(
            "remove",
            "remove a directory from the database",
        ))
}

fn printenv(description: &str) -> CommandSpec {
    CommandSpec::new("printenv", description)
        .with_option(OptionSpec::new("--null", "end output with a null byte"))
        .with_generator(positional_generator(GeneratorKind::EnvironmentVariables, 0))
}

fn fd(description: &str) -> CommandSpec {
    CommandSpec::new("fd", description)
        .with_option(value_option("--extension", "filter by file extension").with_alias("-e"))
        .with_generator(option_generator(GeneratorKind::FileTypes, "--extension"))
}

fn systemctl(description: &str) -> CommandSpec {
    CommandSpec::new("systemctl", description)
        .with_option(OptionSpec::new("--user", "operate on the user service manager").global(true))
        .with_option(
            OptionSpec::new("--system", "operate on the system service manager").global(true),
        )
        .with_option(global_value_option("-H", "operate on a remote host").with_alias("--host"))
        .with_option(
            global_value_option("-M", "operate on a local container").with_alias("--machine"),
        )
        .with_option(OptionSpec::new("--no-pager", "disable paging").global(true))
        .with_option(terminal_option(
            "--version",
            "print systemd version information",
        ))
        .with_subcommand(service_command("start", "start units"))
        .with_subcommand(service_command("stop", "stop units"))
        .with_subcommand(service_command("restart", "restart units"))
        .with_subcommand(service_command("reload", "reload unit configuration"))
        .with_subcommand(
            service_command("status", "show unit status")
                .with_option(OptionSpec::new("-l", "show complete log lines").with_alias("--full")),
        )
        .with_subcommand(
            service_command("enable", "enable units")
                .with_option(OptionSpec::new("--now", "also start units")),
        )
        .with_subcommand(
            service_command("disable", "disable units")
                .with_option(OptionSpec::new("--now", "also stop units")),
        )
        .with_subcommand(service_command("mask", "mask units"))
        .with_subcommand(service_command("unmask", "unmask units"))
        .with_subcommand(service_command(
            "is-active",
            "test whether units are active",
        ))
        .with_subcommand(service_command(
            "is-enabled",
            "test whether units are enabled",
        ))
        .with_subcommand(no_positionals(
            "daemon-reload",
            "reload manager configuration",
        ))
        .with_subcommand(
            CommandSpec::new("list-units", "list loaded units")
                .with_option(value_option("-t", "filter by unit type").with_alias("--type"))
                .with_option(value_option("--state", "filter by unit state"))
                .with_option(OptionSpec::new("-a", "include inactive units").with_alias("--all")),
        )
        .with_subcommand(CommandSpec::new(
            "list-unit-files",
            "list installed unit files",
        ))
        .with_subcommand(service_command("show", "show unit properties"))
        .with_subcommand(service_command("cat", "show unit files"))
        .with_subcommand(service_command("edit", "edit unit override files"))
}

fn value_option(name: &str, description: &str) -> OptionSpec {
    OptionSpec::new(name, description).takes_value(true)
}

fn positional_generator(kind: GeneratorKind, index: usize) -> GeneratorSpec {
    GeneratorSpec::new(kind, GeneratorTarget::Positional(index))
}

fn option_generator(kind: GeneratorKind, name: &str) -> GeneratorSpec {
    GeneratorSpec::new(kind, GeneratorTarget::OptionValue(name.to_owned()))
}

fn filesystem_generator(
    target: GeneratorTarget,
    directory_only: bool,
    extensions: &[&str],
) -> GeneratorSpec {
    GeneratorSpec::new(
        GeneratorKind::Filesystem(FilesystemGenerator {
            directory_only,
            extensions: extensions
                .iter()
                .map(|extension| (*extension).to_owned())
                .collect(),
            ..FilesystemGenerator::default()
        }),
        target,
    )
}

fn positional_files(index: usize) -> GeneratorSpec {
    filesystem_generator(GeneratorTarget::Positional(index), false, &[])
}

fn option_files(name: &str, extensions: &[&str]) -> GeneratorSpec {
    filesystem_generator(
        GeneratorTarget::OptionValue(name.to_owned()),
        false,
        extensions,
    )
}

fn option_directories(name: &str) -> GeneratorSpec {
    filesystem_generator(GeneratorTarget::OptionValue(name.to_owned()), true, &[])
}

fn global_value_option(name: &str, description: &str) -> OptionSpec {
    value_option(name, description).global(true)
}

fn terminal_option(name: &str, description: &str) -> OptionSpec {
    OptionSpec::new(name, description).with_insertion(InsertionBehavior::Exact)
}

fn no_positionals(name: &str, description: &str) -> CommandSpec {
    CommandSpec::new(name, description).with_max_positionals(0)
}

fn service_command(name: &str, description: &str) -> CommandSpec {
    CommandSpec::new(name, description)
        .with_generator(positional_generator(GeneratorKind::Services, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOTS: [&str; 23] = [
        "docker",
        "npm",
        "pnpm",
        "yarn",
        "bun",
        "pip",
        "cargo",
        "go",
        "mvn",
        "cmake",
        "git",
        "apt",
        "tar",
        "vim",
        "jq",
        "just",
        "make",
        "ssh",
        "kill",
        "systemctl",
        "zoxide",
        "printenv",
        "fd",
    ];

    #[test]
    fn every_representative_root_is_available_and_valid() {
        for root in ROOTS {
            let candidate = spec(root, "representative command")
                .unwrap_or_else(|| panic!("missing representative spec for {root}"));
            assert_eq!(candidate.name, root);
            candidate
                .validate()
                .unwrap_or_else(|error| panic!("invalid representative spec for {root}: {error}"));
            assert!(
                !candidate.subcommands.is_empty() || !candidate.options.is_empty(),
                "representative spec for {root} has no useful structure"
            );
            assert!(
                generator_count(&candidate) > 0,
                "representative spec for {root} has no generator declarations"
            );
        }
    }

    #[test]
    fn unknown_root_has_no_representative_spec() {
        assert!(spec("greendale", "unknown command").is_none());
    }

    #[test]
    fn representative_specs_include_recursive_alias_and_insertion_metadata() {
        let git = spec("git", "version control").unwrap();
        let remote = git
            .subcommands
            .iter()
            .find(|command| command.name == "remote")
            .unwrap();
        let remove_command = remote
            .subcommands
            .iter()
            .find(|command| command.name == "remove")
            .unwrap();
        assert!(remove_command.aliases.iter().any(|alias| alias == "rm"));

        let apt = spec("apt", "package manager").unwrap();
        let update = apt
            .subcommands
            .iter()
            .find(|command| command.name == "update")
            .unwrap();
        assert_eq!(update.max_positionals, Some(0));

        let cmake = spec("cmake", "build generator").unwrap();
        let version = cmake
            .options
            .iter()
            .find(|option| option.name == "--version")
            .unwrap();
        assert_eq!(version.insertion, InsertionBehavior::Exact);
    }

    #[test]
    fn representative_generator_targets_cover_local_dynamic_values() {
        assert_generator(
            "docker",
            &["run"],
            &positional_generator(GeneratorKind::DockerImages, 0),
        );
        assert_generator(
            "docker",
            &["exec"],
            &positional_generator(GeneratorKind::DockerContainers, 0),
        );
        assert_generator(
            "npm",
            &["run"],
            &positional_generator(GeneratorKind::PackageScripts, 0),
        );
        for (root, path) in [
            ("pip", &["install"][..]),
            ("cargo", &["add"][..]),
            ("go", &["get"][..]),
            ("apt", &["install"][..]),
        ] {
            assert_generator(
                root,
                path,
                &positional_generator(GeneratorKind::Packages, 0),
            );
        }
        assert_generator("mvn", &[], &option_files("-f", &["xml"]));
        assert_generator("cmake", &[], &option_directories("-S"));
        assert_generator(
            "git",
            &["checkout"],
            &positional_generator(GeneratorKind::GitBranches, 0),
        );
        assert_generator(
            "git",
            &["remote", "remove"],
            &positional_generator(GeneratorKind::GitRemotes, 0),
        );
        assert_generator(
            "git",
            &["stash", "pop"],
            &positional_generator(GeneratorKind::GitStashes, 0),
        );
        assert_generator(
            "git",
            &["add"],
            &positional_generator(GeneratorKind::GitFiles, 0),
        );
        assert_generator(
            "git",
            &["log"],
            &positional_generator(GeneratorKind::GitCommits, 0),
        );
        assert_generator(
            "git",
            &["tag"],
            &positional_generator(GeneratorKind::GitTags, 0),
        );
        assert_generator(
            "tar",
            &[],
            &option_files("-f", &["tar", "tgz", "gz", "bz2", "xz"]),
        );
        assert_generator("vim", &[], &positional_files(0));
        assert_generator("jq", &[], &positional_files(1));
        assert_generator(
            "just",
            &[],
            &positional_generator(GeneratorKind::JustRecipes, 0),
        );
        assert_generator(
            "systemctl",
            &["start"],
            &positional_generator(GeneratorKind::Services, 0),
        );
    }

    #[test]
    fn dynamic_parity_generators_are_reachable() {
        for root in ["pnpm", "yarn", "bun"] {
            assert_generator(
                root,
                &["run"],
                &positional_generator(GeneratorKind::PackageScripts, 0),
            );
        }
        assert_generator(
            "make",
            &[],
            &positional_generator(GeneratorKind::MakeTargets, 0),
        );
        assert_generator(
            "ssh",
            &[],
            &positional_generator(GeneratorKind::SshHosts, 0),
        );
        assert_generator(
            "zoxide",
            &["query"],
            &positional_generator(GeneratorKind::ZoxideDirectories, 0),
        );
        assert_generator(
            "kill",
            &[],
            &positional_generator(GeneratorKind::Processes, 0),
        );
        assert_generator(
            "printenv",
            &[],
            &positional_generator(GeneratorKind::EnvironmentVariables, 0),
        );
        assert_generator(
            "fd",
            &[],
            &option_generator(GeneratorKind::FileTypes, "--extension"),
        );
    }

    fn generator_count(command: &CommandSpec) -> usize {
        command.generators.len()
            + command
                .subcommands
                .iter()
                .map(generator_count)
                .sum::<usize>()
    }

    fn assert_generator(root: &str, path: &[&str], expected: &GeneratorSpec) {
        let root_spec = spec(root, "representative command").unwrap();
        let mut command = &root_spec;
        for segment in path {
            command = command
                .subcommands
                .iter()
                .find(|candidate| candidate.name == *segment)
                .unwrap_or_else(|| panic!("missing command path {root} {}", path.join(" ")));
        }
        assert!(
            command.generators.contains(expected),
            "missing generator {expected:?} at {root} {}",
            path.join(" ")
        );
    }
}
