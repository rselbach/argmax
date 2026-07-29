use crate::completion::{CommandSpec, InsertionBehavior, OptionSpec};

pub(super) fn spec(name: &str, description: &str) -> Option<CommandSpec> {
    Some(match name {
        "docker" => docker(description),
        "npm" => npm(description),
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
        "systemctl" => systemctl(description),
        _ => return None,
    })
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
                .with_option(value_option("--name", "assign a container name")),
        )
        .with_subcommand(
            CommandSpec::new("exec", "run a command in a running container")
                .with_option(
                    OptionSpec::new("-i", "keep standard input open").with_alias("--interactive"),
                )
                .with_option(
                    OptionSpec::new("-t", "allocate a pseudo-terminal").with_alias("--tty"),
                ),
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
        .with_subcommand(CommandSpec::new("run", "run a package script").with_option(
            OptionSpec::new("--if-present", "succeed when the script is absent"),
        ))
        .with_subcommand(CommandSpec::new("exec", "run a package-provided command"))
        .with_subcommand(CommandSpec::new("init", "create a package manifest"))
        .with_subcommand(CommandSpec::new("publish", "publish a package"))
        .with_subcommand(no_positionals("outdated", "list outdated dependencies"))
        .with_subcommand(no_positionals("test", "run the package test script"))
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
                .with_option(OptionSpec::new("-U", "upgrade packages").with_alias("--upgrade")),
        )
        .with_subcommand(
            CommandSpec::new("uninstall", "remove installed packages")
                .with_option(OptionSpec::new("-y", "confirm all removals").with_alias("--yes")),
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
        .with_subcommand(CommandSpec::new("show", "show package information"))
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
                .with_option(value_option("--features", "enable dependency features")),
        )
        .with_subcommand(CommandSpec::new("remove", "remove dependencies"))
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
        .with_subcommand(CommandSpec::new("get", "resolve and add dependencies"))
        .with_subcommand(CommandSpec::new("install", "compile and install packages"))
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
                ),
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
                .with_option(OptionSpec::new("-m", "move or rename a branch").with_alias("--move")),
        )
        .with_subcommand(
            CommandSpec::new("checkout", "switch branches or restore files")
                .with_option(value_option("-b", "create and switch to a branch")),
        )
        .with_subcommand(CommandSpec::new("switch", "switch branches").with_option(
            value_option("-c", "create and switch to a branch").with_alias("--create"),
        ))
        .with_subcommand(
            CommandSpec::new("log", "show commit logs")
                .with_option(
                    value_option("-n", "limit the number of commits").with_alias("--max-count"),
                )
                .with_option(OptionSpec::new("--oneline", "show one line per commit")),
        )
        .with_subcommand(
            no_positionals("status", "show the working tree status").with_option(
                OptionSpec::new("-s", "show short-format status").with_alias("--short"),
            ),
        )
        .with_subcommand(
            CommandSpec::new("remote", "manage tracked repositories")
                .with_subcommand(CommandSpec::new("add", "add a remote").with_max_positionals(2))
                .with_subcommand(
                    CommandSpec::new("get-url", "read remote URLs").with_max_positionals(1),
                )
                .with_subcommand(
                    CommandSpec::new("remove", "remove a remote")
                        .with_alias("rm")
                        .with_max_positionals(1),
                )
                .with_subcommand(
                    CommandSpec::new("rename", "rename a remote").with_max_positionals(2),
                )
                .with_subcommand(CommandSpec::new("set-url", "change remote URLs"))
                .with_subcommand(
                    CommandSpec::new("show", "inspect a remote").with_max_positionals(1),
                ),
        )
        .with_subcommand(
            CommandSpec::new("stash", "stash working tree changes")
                .with_subcommand(CommandSpec::new("push", "save local modifications"))
                .with_subcommand(no_positionals("list", "list stashed changes"))
                .with_subcommand(
                    CommandSpec::new("show", "inspect a stashed change").with_max_positionals(1),
                )
                .with_subcommand(
                    CommandSpec::new("apply", "apply a stashed change").with_max_positionals(1),
                )
                .with_subcommand(
                    CommandSpec::new("drop", "remove a stashed change").with_max_positionals(1),
                )
                .with_subcommand(no_positionals("pop", "apply and remove a stashed change")),
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
        .with_subcommand(CommandSpec::new("install", "install packages"))
        .with_subcommand(CommandSpec::new("remove", "remove packages"))
        .with_subcommand(CommandSpec::new(
            "purge",
            "remove packages and configuration",
        ))
        .with_subcommand(CommandSpec::new(
            "autoremove",
            "remove unneeded dependencies",
        ))
        .with_subcommand(CommandSpec::new("search", "search package descriptions"))
        .with_subcommand(CommandSpec::new("show", "show package details"))
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
        .with_subcommand(CommandSpec::new("start", "start units"))
        .with_subcommand(CommandSpec::new("stop", "stop units"))
        .with_subcommand(CommandSpec::new("restart", "restart units"))
        .with_subcommand(CommandSpec::new("reload", "reload unit configuration"))
        .with_subcommand(
            CommandSpec::new("status", "show unit status")
                .with_option(OptionSpec::new("-l", "show complete log lines").with_alias("--full")),
        )
        .with_subcommand(
            CommandSpec::new("enable", "enable units")
                .with_option(OptionSpec::new("--now", "also start units")),
        )
        .with_subcommand(
            CommandSpec::new("disable", "disable units")
                .with_option(OptionSpec::new("--now", "also stop units")),
        )
        .with_subcommand(CommandSpec::new("mask", "mask units"))
        .with_subcommand(CommandSpec::new("unmask", "unmask units"))
        .with_subcommand(CommandSpec::new(
            "is-active",
            "test whether units are active",
        ))
        .with_subcommand(CommandSpec::new(
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
        .with_subcommand(CommandSpec::new("show", "show unit properties"))
        .with_subcommand(CommandSpec::new("cat", "show unit files"))
        .with_subcommand(CommandSpec::new("edit", "edit unit override files"))
}

fn value_option(name: &str, description: &str) -> OptionSpec {
    OptionSpec::new(name, description).takes_value(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    const ROOTS: [&str; 14] = [
        "docker",
        "npm",
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
        "systemctl",
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
}
