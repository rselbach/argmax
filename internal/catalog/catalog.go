// Package catalog compiles the bundled command specifications into the
// binary: hand-tuned Go specs for the highest-value commands plus an
// embedded data bundle generated from the PRD section 18 list covering
// all 567 catalog entries. The registry is validated in CI.
package catalog

import (
	"github.com/rselbach/argmax/internal/complete"
	"github.com/rselbach/argmax/internal/generators"
)

// specs returns the hand-tuned Go specifications; the rest of the catalog
// is loaded from the embedded data bundle (see load.go).
func specs() []*complete.Spec {
	list := []*complete.Spec{
		gitSpec(),
		goSpec(),
		dockerSpec(),
		kubectlSpec(),
		cargoSpec(),
		makeSpec(),
		justSpec(),
		chmodSpec(),
		cdSpec(),
	}
	list = append(list, nodeSpecs()...)
	list = append(list, sshSpecs()...)
	list = append(list, systemSpecs()...)
	list = append(list, packageManagerSpecs()...)
	list = append(list, networkSpecs()...)
	list = append(list, simpleSpecs()...)
	return list
}

// networkSpecs covers network and process tools absent from the data
// corpus.
func networkSpecs() []*complete.Spec {
	return []*complete.Spec{
		{Name: "pgrep", Description: "find processes by name", Icon: "process",
			Generator: generators.Processes(), Options: []complete.Option{
				{Names: []string{"-f"}, Description: "match against the full argument list"},
				{Names: []string{"-l"}, Description: "list the process name with the PID"},
				{Names: []string{"-x"}, Description: "match the process name exactly"},
				{Names: []string{"-u"}, Description: "match processes of the given user", TakesArg: true},
				{Names: []string{"-n"}, Description: "select only the newest matching process"},
			}},
		{Name: "ifconfig", Description: "configure network interfaces", Icon: "process",
			Options: []complete.Option{
				{Names: []string{"-a"}, Description: "show all interfaces"},
				{Names: []string{"-l"}, Description: "list interface names only"},
				{Names: []string{"-u"}, Description: "show up interfaces only"},
				{Names: []string{"-d"}, Description: "show down interfaces only"},
			}},
		{Name: "ip", Description: "show and manipulate routing and devices", Icon: "process",
			Subcommands: []*complete.Spec{
				{Name: "address", Aliases: []string{"addr", "a"}, Description: "protocol addresses"},
				{Name: "link", Aliases: []string{"l"}, Description: "network devices"},
				{Name: "route", Aliases: []string{"r"}, Description: "routing table entries"},
				{Name: "neighbor", Aliases: []string{"neigh"}, Description: "ARP or NDISC cache entries"},
				{Name: "netns", Description: "network namespaces"},
				{Name: "rule", Description: "routing policy rules"},
			},
			Options: []complete.Option{
				{Names: []string{"-4"}, Description: "IPv4 only"},
				{Names: []string{"-6"}, Description: "IPv6 only"},
				{Names: []string{"-br", "-brief"}, Description: "brief tabular output"},
				{Names: []string{"-j", "-json"}, Description: "JSON output"},
			}},
		{Name: "ss", Description: "socket statistics", Icon: "process",
			Options: []complete.Option{
				{Names: []string{"-t"}, Description: "TCP sockets"},
				{Names: []string{"-u"}, Description: "UDP sockets"},
				{Names: []string{"-l"}, Description: "listening sockets"},
				{Names: []string{"-p"}, Description: "show owning processes"},
				{Names: []string{"-n"}, Description: "numeric addresses and ports"},
				{Names: []string{"-a"}, Description: "all sockets"},
			}},
		{Name: "netstat", Description: "network connections and statistics", Icon: "process",
			Options: []complete.Option{
				{Names: []string{"-a"}, Description: "all sockets"},
				{Names: []string{"-n"}, Description: "numeric addresses"},
				{Names: []string{"-r"}, Description: "routing table"},
				{Names: []string{"-l"}, Description: "listening sockets"},
				{Names: []string{"-p"}, Description: "show owning processes"},
				{Names: []string{"-i"}, Description: "interface statistics"},
			}},
		{Name: "nslookup", Description: "query DNS name servers", Icon: "process",
			Options: []complete.Option{
				{Names: []string{"-type=A"}, Description: "query A records"},
				{Names: []string{"-type=AAAA"}, Description: "query AAAA records"},
				{Names: []string{"-type=MX"}, Description: "query MX records"},
				{Names: []string{"-type=TXT"}, Description: "query TXT records"},
				{Names: []string{"-type=NS"}, Description: "query NS records"},
			}},
		{Name: "javac", Description: "Java compiler", Icon: "package",
			Generator: generators.FilesWithExt("java"), Options: []complete.Option{
				{Names: []string{"-d"}, Description: "output directory for class files", TakesArg: true},
				{Names: []string{"-cp", "-classpath", "--class-path"}, Description: "class search path", TakesArg: true},
				{Names: []string{"--release"}, Description: "compile for the given Java release", TakesArg: true},
				{Names: []string{"-verbose"}, Description: "verbose compiler output"},
				{Names: []string{"-Werror"}, Description: "treat warnings as errors"},
			}},
		{Name: "flatpak", Description: "application sandboxing and distribution", Icon: "package",
			Subcommands: []*complete.Spec{
				{Name: "install", Description: "install an application or runtime"},
				{Name: "uninstall", Aliases: []string{"remove"}, Description: "uninstall an application"},
				{Name: "update", Description: "update installed applications"},
				{Name: "run", Description: "run an application"},
				{Name: "list", Description: "list installed applications"},
				{Name: "search", Description: "search for applications"},
				{Name: "info", Description: "show application details"},
				{Name: "remotes", Description: "list configured remotes"},
				{Name: "remote-add", Description: "add a remote repository"},
			}},
		{Name: "snap", Description: "package management for snaps", Icon: "package",
			Subcommands: []*complete.Spec{
				{Name: "install", Description: "install a snap"},
				{Name: "remove", Description: "remove a snap"},
				{Name: "refresh", Description: "update installed snaps"},
				{Name: "list", Description: "list installed snaps"},
				{Name: "find", Description: "search the store"},
				{Name: "info", Description: "show snap details"},
				{Name: "start", Description: "start snap services"},
				{Name: "stop", Description: "stop snap services"},
				{Name: "services", Description: "list snap services"},
			}},
	}
}

func gitSpec() *complete.Spec {
	branchOpts := []complete.Option{
		{Names: []string{"-b"}, Description: "create and switch to a new branch", TakesArg: true},
		{Names: []string{"-B"}, Description: "create or reset and switch to a branch", TakesArg: true},
	}
	return &complete.Spec{
		Name: "git", Description: "distributed version control", Icon: "git",
		Options: []complete.Option{
			{Names: []string{"-C"}, Description: "run as if started in the given path", TakesArg: true, Generator: generators.Directories()},
			{Names: []string{"-c"}, Description: "set a configuration value", TakesArg: true},
			{Names: []string{"--no-pager"}, Description: "do not pipe output into a pager"},
		},
		Subcommands: []*complete.Spec{
			{Name: "status", Description: "show the working tree status", Options: []complete.Option{
				{Names: []string{"-s", "--short"}, Description: "short-format output"},
				{Names: []string{"-b", "--branch"}, Description: "show branch information"},
			}},
			{Name: "add", Description: "add file contents to the index", Generator: generators.Files(), Options: []complete.Option{
				{Names: []string{"-A", "--all"}, Description: "add all tracked and untracked changes"},
				{Names: []string{"-p", "--patch"}, Description: "interactively choose hunks"},
				{Names: []string{"-u", "--update"}, Description: "update tracked files only"},
			}},
			{Name: "commit", Description: "record changes to the repository", Options: []complete.Option{
				{Names: []string{"-m", "--message"}, Description: "commit message", TakesArg: true},
				{Names: []string{"-a", "--all"}, Description: "commit all changed files"},
				{Names: []string{"--amend"}, Description: "amend the previous commit"},
				{Names: []string{"--no-verify"}, Description: "skip pre-commit hooks"},
			}},
			{Name: "checkout", Description: "switch branches or restore files",
				Generator: generators.GitCheckoutTargets(), Options: branchOpts},
			{Name: "switch", Description: "switch branches", Generator: generators.GitBranches(), Options: []complete.Option{
				{Names: []string{"-c", "--create"}, Description: "create and switch to a new branch", TakesArg: true},
				{Names: []string{"-"}, Description: "switch to the previous branch"},
			}},
			{Name: "branch", Description: "list, create, or delete branches", Generator: generators.GitBranches(), Options: []complete.Option{
				{Names: []string{"-d", "--delete"}, Description: "delete a merged branch"},
				{Names: []string{"-D"}, Description: "force-delete a branch"},
				{Names: []string{"-m", "--move"}, Description: "rename a branch"},
				{Names: []string{"-a", "--all"}, Description: "list remote-tracking and local branches"},
			}},
			{Name: "merge", Description: "join development histories", Generator: generators.GitBranches(), Options: []complete.Option{
				{Names: []string{"--no-ff"}, Description: "always create a merge commit"},
				{Names: []string{"--abort"}, Description: "abort the current merge"},
				{Names: []string{"--continue"}, Description: "continue after resolving conflicts"},
			}},
			{Name: "rebase", Description: "reapply commits on top of another base", Generator: generators.GitBranches(), Options: []complete.Option{
				{Names: []string{"--abort"}, Description: "abort the rebase"},
				{Names: []string{"--continue"}, Description: "continue the rebase"},
				{Names: []string{"--onto"}, Description: "rebase onto the given base", TakesArg: true},
			}},
			{Name: "push", Description: "update remote refs", Generator: generators.GitPushPull(), MaxArgs: 2, Options: []complete.Option{
				{Names: []string{"-u", "--set-upstream"}, Description: "set the upstream for the branch"},
				{Names: []string{"-f", "--force-with-lease"}, Description: "force push, refusing to clobber others"},
				{Names: []string{"--tags"}, Description: "push tags"},
			}},
			{Name: "pull", Description: "fetch and integrate from a remote", Generator: generators.GitPushPull(), MaxArgs: 2, Options: []complete.Option{
				{Names: []string{"--rebase"}, Description: "rebase instead of merge"},
				{Names: []string{"--ff-only"}, Description: "fast-forward only"},
			}},
			{Name: "fetch", Description: "download objects and refs", Generator: generators.GitPushPull(), MaxArgs: 1, Options: []complete.Option{
				{Names: []string{"--all"}, Description: "fetch all remotes"},
				{Names: []string{"-p", "--prune"}, Description: "prune deleted remote branches"},
			}},
			{Name: "reset", Description: "reset current HEAD", Generator: generators.GitResetTargets(), Options: []complete.Option{
				{Names: []string{"--soft"}, Description: "keep index and working tree"},
				{Names: []string{"--mixed"}, Description: "keep working tree, reset index"},
			}},
			{Name: "show", Description: "show objects", Generator: generators.GitShowTargets()},
			{Name: "log", Description: "show commit logs", Generator: generators.GitRefs(), Options: []complete.Option{
				{Names: []string{"--oneline"}, Description: "one line per commit"},
				{Names: []string{"-p", "--patch"}, Description: "show diffs"},
				{Names: []string{"--graph"}, Description: "draw the commit graph"},
			}},
			{Name: "diff", Description: "show changes", Generator: generators.GitRefs(), Options: []complete.Option{
				{Names: []string{"--staged", "--cached"}, Description: "diff the staged changes"},
				{Names: []string{"--stat"}, Description: "show a diffstat"},
			}},
			{Name: "stash", Description: "stash changes away", Subcommands: []*complete.Spec{
				{Name: "push", Description: "save local modifications"},
				{Name: "pop", Description: "apply and drop the latest stash"},
				{Name: "list", Description: "list stashes"},
				{Name: "apply", Description: "apply a stash"},
				{Name: "drop", Description: "drop a stash"},
			}},
			{Name: "remote", Description: "manage remotes", Subcommands: []*complete.Spec{
				{Name: "add", Description: "add a remote"},
				{Name: "remove", Description: "remove a remote"},
				{Name: "rename", Description: "rename a remote"},
				{Name: "show", Description: "show a remote"},
				{Name: "-v", Description: "list remotes verbosely"},
			}},
			{Name: "tag", Description: "create, list, or delete tags", Generator: generators.GitRefs()},
			{Name: "init", Description: "create an empty repository"},
			{Name: "clone", Description: "clone a repository", Options: []complete.Option{
				{Names: []string{"--depth"}, Description: "create a shallow clone", TakesArg: true},
			}},
			{Name: "restore", Description: "restore working tree files", Generator: generators.Files(), Options: []complete.Option{
				{Names: []string{"--staged"}, Description: "restore the index"},
			}},
			{Name: "worktree", Description: "manage worktrees", Subcommands: []*complete.Spec{
				{Name: "add", Description: "create a worktree", Generator: generators.Directories()},
				{Name: "list", Description: "list worktrees"},
				{Name: "remove", Description: "remove a worktree"},
			}},
			{Name: "cherry-pick", Description: "apply existing commits", Generator: generators.GitRefs()},
			{Name: "revert", Description: "revert existing commits", Generator: generators.GitRefs()},
			{Name: "blame", Description: "show line-by-line authorship", Generator: generators.Files()},
			{Name: "bisect", Description: "binary-search for a bad commit", Subcommands: []*complete.Spec{
				{Name: "start", Description: "start bisecting"},
				{Name: "good", Description: "mark a commit good"},
				{Name: "bad", Description: "mark a commit bad"},
				{Name: "reset", Description: "finish bisecting"},
			}},
			{Name: "config", Description: "get and set options", Subcommands: []*complete.Spec{
				{Name: "get", Description: "read a value"},
				{Name: "set", Description: "write a value"},
				{Name: "list", Description: "list all values", Aliases: []string{"-l"}},
			}},
		},
	}
}

func goSpec() *complete.Spec {
	goFiles := generators.FilesWithExt("go")
	return &complete.Spec{
		Name: "go", Description: "Go toolchain", Icon: "go",
		Subcommands: []*complete.Spec{
			{Name: "build", Description: "compile packages", Generator: goFiles, Options: []complete.Option{
				{Names: []string{"-o"}, Description: "output file", TakesArg: true},
				{Names: []string{"-race"}, Description: "enable the race detector"},
			}},
			{Name: "run", Description: "compile and run a package", Generator: goFiles},
			{Name: "test", Description: "test packages", Generator: goFiles, Options: []complete.Option{
				{Names: []string{"-run"}, Description: "run tests matching a pattern", TakesArg: true},
				{Names: []string{"-v"}, Description: "verbose output"},
				{Names: []string{"-race"}, Description: "enable the race detector"},
				{Names: []string{"-count"}, Description: "run each test n times", TakesArg: true},
				{Names: []string{"-cover"}, Description: "enable coverage analysis"},
			}},
			{Name: "vet", Description: "report likely mistakes"},
			{Name: "fmt", Description: "gofmt packages"},
			{Name: "get", Description: "add dependencies"},
			{Name: "install", Description: "compile and install packages"},
			{Name: "mod", Description: "module maintenance", Subcommands: []*complete.Spec{
				{Name: "tidy", Description: "add missing and remove unused modules"},
				{Name: "download", Description: "download modules to the cache"},
				{Name: "vendor", Description: "vendor dependencies"},
				{Name: "init", Description: "initialize a module"},
				{Name: "why", Description: "explain why a module is needed"},
			}},
			{Name: "work", Description: "workspace maintenance", Subcommands: []*complete.Spec{
				{Name: "init", Description: "initialize a workspace"},
				{Name: "use", Description: "add a module directory", Generator: generators.Directories()},
				{Name: "sync", Description: "sync workspace build list"},
			}},
			{Name: "generate", Description: "run code generators"},
			{Name: "doc", Description: "show documentation"},
			{Name: "env", Description: "print Go environment"},
			{Name: "version", Description: "print Go version"},
			{Name: "clean", Description: "remove object files"},
			{Name: "tool", Description: "run a Go tool"},
		},
	}
}

func dockerSpec() *complete.Spec {
	return &complete.Spec{
		Name: "docker", Description: "container runtime", Icon: "docker",
		Subcommands: []*complete.Spec{
			{Name: "ps", Description: "list containers", Options: []complete.Option{
				{Names: []string{"-a", "--all"}, Description: "include stopped containers"},
			}},
			{Name: "run", Description: "run a command in a new container", Generator: generators.DockerImages(), Options: []complete.Option{
				{Names: []string{"-it"}, Description: "interactive with a TTY"},
				{Names: []string{"--rm"}, Description: "remove the container on exit"},
				{Names: []string{"-d", "--detach"}, Description: "run in the background"},
				{Names: []string{"-p", "--publish"}, Description: "publish a port", TakesArg: true},
				{Names: []string{"-v", "--volume"}, Description: "bind mount a volume", TakesArg: true},
				{Names: []string{"-e", "--env"}, Description: "set an environment variable", TakesArg: true},
			}},
			{Name: "exec", Description: "run a command in a running container", Generator: generators.DockerContainers(true), Options: []complete.Option{
				{Names: []string{"-it"}, Description: "interactive with a TTY"},
			}},
			{Name: "logs", Description: "fetch container logs", Generator: generators.DockerContainers(true), Options: []complete.Option{
				{Names: []string{"-f", "--follow"}, Description: "follow output"},
				{Names: []string{"--tail"}, Description: "lines from the end", TakesArg: true},
			}},
			{Name: "stop", Description: "stop containers", Generator: generators.DockerContainers(true)},
			{Name: "start", Description: "start containers", Generator: generators.DockerContainers(false)},
			{Name: "restart", Description: "restart containers", Generator: generators.DockerContainers(false)},
			{Name: "rm", Description: "remove containers", Generator: generators.DockerContainers(false)},
			{Name: "rmi", Description: "remove images", Generator: generators.DockerImages()},
			{Name: "images", Description: "list images"},
			{Name: "pull", Description: "download an image"},
			{Name: "push", Description: "upload an image", Generator: generators.DockerImages()},
			{Name: "build", Description: "build an image", Generator: generators.Directories(), Options: []complete.Option{
				{Names: []string{"-t", "--tag"}, Description: "name and tag the image", TakesArg: true},
				{Names: []string{"-f", "--file"}, Description: "Dockerfile path", TakesArg: true, Generator: generators.Files()},
			}},
			{Name: "inspect", Description: "inspect objects", Generator: generators.DockerInspectTargets()},
			{Name: "compose", Description: "multi-container applications", Subcommands: []*complete.Spec{
				{Name: "up", Description: "create and start services", Options: []complete.Option{
					{Names: []string{"-d", "--detach"}, Description: "run in the background"},
					{Names: []string{"--build"}, Description: "build images before starting"},
				}},
				{Name: "down", Description: "stop and remove services"},
				{Name: "ps", Description: "list service containers"},
				{Name: "logs", Description: "view service logs"},
				{Name: "exec", Description: "run a command in a service"},
				{Name: "build", Description: "build service images"},
				{Name: "restart", Description: "restart services"},
				{Name: "pull", Description: "pull service images"},
			}, Options: []complete.Option{
				{Names: []string{"-f", "--file"}, Description: "compose file", TakesArg: true, Generator: generators.FilesWithExt("yml", "yaml")},
			}},
			{Name: "volume", Description: "manage volumes", Subcommands: []*complete.Spec{
				{Name: "ls", Description: "list volumes"},
				{Name: "rm", Description: "remove volumes"},
				{Name: "prune", Description: "remove unused volumes"},
			}},
			{Name: "network", Description: "manage networks", Subcommands: []*complete.Spec{
				{Name: "ls", Description: "list networks"},
				{Name: "rm", Description: "remove networks"},
				{Name: "create", Description: "create a network"},
			}},
			{Name: "system", Description: "manage Docker", Subcommands: []*complete.Spec{
				{Name: "prune", Description: "remove unused data"},
				{Name: "df", Description: "show disk usage"},
			}},
		},
	}
}

func kubectlSpec() *complete.Spec {
	resources := []*complete.Spec{
		{Name: "pods", Aliases: []string{"pod", "po"}, Description: "pods"},
		{Name: "deployments", Aliases: []string{"deployment", "deploy"}, Description: "deployments"},
		{Name: "services", Aliases: []string{"service", "svc"}, Description: "services"},
		{Name: "nodes", Aliases: []string{"node", "no"}, Description: "nodes"},
		{Name: "namespaces", Aliases: []string{"namespace", "ns"}, Description: "namespaces"},
		{Name: "configmaps", Aliases: []string{"configmap", "cm"}, Description: "config maps"},
		{Name: "secrets", Aliases: []string{"secret"}, Description: "secrets"},
		{Name: "ingresses", Aliases: []string{"ingress", "ing"}, Description: "ingresses"},
	}
	nsOpt := complete.Option{Names: []string{"-n", "--namespace"}, Description: "namespace", TakesArg: true}
	return &complete.Spec{
		Name: "kubectl", Description: "Kubernetes CLI", Icon: "kubernetes",
		Options: []complete.Option{nsOpt, {Names: []string{"--context"}, Description: "kubeconfig context", TakesArg: true}},
		Subcommands: []*complete.Spec{
			{Name: "get", Description: "display resources", Subcommands: resources, Options: []complete.Option{
				{Names: []string{"-o", "--output"}, Description: "output format", TakesArg: true},
				{Names: []string{"-w", "--watch"}, Description: "watch for changes"},
				{Names: []string{"-A", "--all-namespaces"}, Description: "all namespaces"},
			}},
			{Name: "describe", Description: "show resource details", Subcommands: resources},
			{Name: "delete", Description: "delete resources", Subcommands: resources},
			{Name: "apply", Description: "apply a configuration", Options: []complete.Option{
				{Names: []string{"-f", "--filename"}, Description: "manifest file", TakesArg: true, Generator: generators.FilesWithExt("yaml", "yml", "json")},
				{Names: []string{"-k", "--kustomize"}, Description: "kustomization directory", TakesArg: true, Generator: generators.Directories()},
			}},
			{Name: "logs", Description: "print pod logs", Options: []complete.Option{
				{Names: []string{"-f", "--follow"}, Description: "follow output"},
				{Names: []string{"-c", "--container"}, Description: "container name", TakesArg: true},
			}},
			{Name: "exec", Description: "execute in a container", Options: []complete.Option{
				{Names: []string{"-it"}, Description: "interactive with a TTY"},
			}},
			{Name: "port-forward", Description: "forward local ports"},
			{Name: "rollout", Description: "manage rollouts", Subcommands: []*complete.Spec{
				{Name: "status", Description: "show rollout status"},
				{Name: "restart", Description: "restart a rollout"},
				{Name: "undo", Description: "undo a rollout"},
			}},
			{Name: "scale", Description: "scale resources"},
			{Name: "config", Description: "modify kubeconfig", Subcommands: []*complete.Spec{
				{Name: "get-contexts", Description: "list contexts"},
				{Name: "use-context", Description: "switch context"},
				{Name: "current-context", Description: "show current context"},
			}},
			{Name: "top", Description: "resource usage", Subcommands: []*complete.Spec{
				{Name: "pods", Description: "pod usage"},
				{Name: "nodes", Description: "node usage"},
			}},
		},
	}
}

func cargoSpec() *complete.Spec {
	return &complete.Spec{
		Name: "cargo", Description: "Rust package manager", Icon: "rust",
		Subcommands: []*complete.Spec{
			{Name: "build", Description: "compile the package", Options: []complete.Option{
				{Names: []string{"--release"}, Description: "optimized build"},
			}},
			{Name: "run", Description: "run the package"},
			{Name: "test", Description: "run tests"},
			{Name: "check", Description: "type-check without codegen"},
			{Name: "clippy", Description: "run lints"},
			{Name: "fmt", Description: "format sources"},
			{Name: "add", Description: "add a dependency"},
			{Name: "remove", Description: "remove a dependency"},
			{Name: "update", Description: "update dependencies"},
			{Name: "doc", Description: "build documentation"},
			{Name: "clean", Description: "remove build artifacts"},
			{Name: "install", Description: "install a binary crate"},
			{Name: "new", Description: "create a package"},
			{Name: "init", Description: "initialize a package"},
			{Name: "publish", Description: "publish to the registry"},
		},
	}
}

func makeSpec() *complete.Spec {
	return &complete.Spec{
		Name: "make", Description: "build automation", Icon: "task",
		Generator: generators.MakeTargets(),
		Options: []complete.Option{
			{Names: []string{"-j", "--jobs"}, Description: "parallel jobs", TakesArg: true},
			{Names: []string{"-C", "--directory"}, Description: "change directory first", TakesArg: true, Generator: generators.Directories()},
			{Names: []string{"-n", "--dry-run"}, Description: "print commands without running"},
			{Names: []string{"-B", "--always-make"}, Description: "unconditionally make targets"},
		},
	}
}

func justSpec() *complete.Spec {
	return &complete.Spec{
		Name: "just", Description: "command runner", Icon: "task",
		Generator: generators.JustRecipes(),
		Options: []complete.Option{
			{Names: []string{"-l", "--list"}, Description: "list recipes"},
			{Names: []string{"-n", "--dry-run"}, Description: "print without running"},
			{Names: []string{"--choose"}, Description: "choose a recipe interactively"},
		},
	}
}

func chmodSpec() *complete.Spec {
	return &complete.Spec{
		Name: "chmod", Description: "change file modes", Icon: "shield",
		Generator: generators.ChmodModes(),
		Options: []complete.Option{
			{Names: []string{"-R"}, Description: "recurse into directories"},
		},
	}
}

func cdSpec() *complete.Spec {
	return &complete.Spec{
		Name: "cd", Description: "change directory", Icon: "folder",
		Generator: generators.Directories(), MaxArgs: 1,
	}
}

func nodeSpecs() []*complete.Spec {
	scripts := generators.PackageScripts()
	runNode := func(name string) *complete.Spec {
		return &complete.Spec{
			Name: name, Description: "JavaScript package manager", Icon: "node",
			Subcommands: []*complete.Spec{
				{Name: "run", Description: "run a package script", Generator: scripts},
				{Name: "install", Aliases: []string{"i"}, Description: "install dependencies"},
				{Name: "add", Description: "add a dependency"},
				{Name: "remove", Aliases: []string{"rm", "uninstall"}, Description: "remove a dependency"},
				{Name: "test", Description: "run the test script"},
				{Name: "init", Description: "create a package.json"},
				{Name: "update", Description: "update dependencies"},
				{Name: "exec", Description: "run a command from a package"},
				{Name: "publish", Description: "publish the package"},
				{Name: "outdated", Description: "list outdated dependencies"},
				{Name: "audit", Description: "audit for vulnerabilities"},
				{Name: "ci", Description: "clean install from lockfile"},
			},
		}
	}
	yarn := runNode("yarn")
	bun := runNode("bun")
	return []*complete.Spec{runNode("npm"), runNode("pnpm"), yarn, bun,
		{Name: "npx", Description: "run a package binary", Icon: "node"},
		{Name: "node", Description: "JavaScript runtime", Icon: "node",
			Generator: generators.FilesWithExt("js", "mjs", "cjs", "ts")},
	}
}

func sshSpecs() []*complete.Spec {
	hosts := generators.SSHHosts()
	return []*complete.Spec{
		{Name: "ssh", Description: "remote login", Icon: "ssh", Generator: hosts, Options: []complete.Option{
			{Names: []string{"-p"}, Description: "port", TakesArg: true},
			{Names: []string{"-i"}, Description: "identity file", TakesArg: true, Generator: generators.Files()},
			{Names: []string{"-L"}, Description: "local port forward", TakesArg: true},
		}},
		{Name: "scp", Description: "secure copy", Icon: "ssh", Generator: hosts, Options: []complete.Option{
			{Names: []string{"-r"}, Description: "copy recursively"},
			{Names: []string{"-P"}, Description: "port", TakesArg: true},
		}},
		{Name: "rsync", Description: "fast incremental transfer", Icon: "ssh", Generator: hosts, Options: []complete.Option{
			{Names: []string{"-a", "--archive"}, Description: "archive mode"},
			{Names: []string{"-v", "--verbose"}, Description: "verbose output"},
			{Names: []string{"-z", "--compress"}, Description: "compress during transfer"},
			{Names: []string{"--delete"}, Description: "delete extraneous destination files"},
		}},
	}
}

func systemSpecs() []*complete.Spec {
	procs := generators.Processes()
	env := generators.EnvVars()
	return []*complete.Spec{
		{Name: "kill", Description: "terminate processes", Icon: "process", Generator: procs, Options: []complete.Option{
			{Names: []string{"-9"}, Description: "SIGKILL"},
			{Names: []string{"-15"}, Description: "SIGTERM"},
			{Names: []string{"-HUP"}, Description: "SIGHUP"},
		}},
		{Name: "killall", Description: "kill processes by name", Icon: "process", Generator: procs},
		{Name: "export", Description: "set environment variables", Icon: "env", Generator: env},
		{Name: "unset", Description: "unset environment variables", Icon: "env", Generator: env},
		{Name: "env", Description: "print the environment", Icon: "env", Generator: env},
		{Name: "printenv", Description: "print environment values", Icon: "env", Generator: env},
		{Name: "systemctl", Description: "control systemd", Icon: "process", Subcommands: []*complete.Spec{
			{Name: "status", Description: "unit status"},
			{Name: "start", Description: "start a unit"},
			{Name: "stop", Description: "stop a unit"},
			{Name: "restart", Description: "restart a unit"},
			{Name: "enable", Description: "enable a unit"},
			{Name: "disable", Description: "disable a unit"},
			{Name: "daemon-reload", Description: "reload systemd configuration"},
		}},
	}
}

func packageManagerSpecs() []*complete.Spec {
	specs := []*complete.Spec{}
	type pm struct {
		name    string
		desc    string
		install string
		remove  string
	}
	for _, m := range []pm{
		{"brew", "Homebrew package manager", "install", "uninstall"},
		{"apt", "Debian package manager", "install", "remove"},
		{"apt-get", "Debian package manager", "install", "remove"},
		{"dnf", "Fedora package manager", "install", "remove"},
		{"yum", "RPM package manager", "install", "remove"},
		{"pacman", "Arch package manager", "-S", "-R"},
		{"yay", "AUR helper", "-S", "-R"},
		{"paru", "AUR helper", "-S", "-R"},
	} {
		installed := generators.InstalledPackages(m.name)
		spec := &complete.Spec{Name: m.name, Description: m.desc, Icon: "package"}
		if m.name == "pacman" || m.name == "yay" || m.name == "paru" {
			spec.Options = []complete.Option{
				{Names: []string{"-S"}, Description: "install packages"},
				{Names: []string{"-R"}, Description: "remove packages"},
				{Names: []string{"-Syu"}, Description: "upgrade the system"},
				{Names: []string{"-Q"}, Description: "query installed packages"},
			}
			spec.Generator = installed
		} else {
			spec.Subcommands = []*complete.Spec{
				{Name: m.install, Description: "install packages"},
				{Name: m.remove, Description: "remove packages", Generator: installed},
				{Name: "update", Description: "refresh package data"},
				{Name: "upgrade", Description: "upgrade packages", Generator: installed},
				{Name: "search", Description: "search packages"},
			}
			if m.name == "brew" {
				spec.Subcommands = append(spec.Subcommands,
					&complete.Spec{Name: "reinstall", Description: "reinstall packages", Generator: installed},
					&complete.Spec{Name: "info", Description: "show package info", Generator: installed},
					&complete.Spec{Name: "list", Description: "list installed packages"},
				)
			}
		}
		specs = append(specs, spec)
	}
	specs = append(specs, &complete.Spec{
		Name: "pip", Aliases: []string{"pip3"}, Description: "Python package manager", Icon: "python",
		Subcommands: []*complete.Spec{
			{Name: "install", Description: "install packages"},
			{Name: "uninstall", Description: "remove packages", Generator: generators.PipPackages()},
			{Name: "list", Description: "list installed packages"},
			{Name: "show", Description: "show package info", Generator: generators.PipPackages()},
			{Name: "freeze", Description: "output installed packages"},
		},
	})
	return specs
}

// simpleSpecs lists common tools completed with files or plain options.
func simpleSpecs() []*complete.Spec {
	files := generators.Files()
	dirs := generators.Directories()
	var out []*complete.Spec
	fileTools := map[string]string{
		"cat": "concatenate files", "bat": "cat with wings", "less": "pager",
		"vim": "text editor", "nvim": "text editor", "nano": "text editor",
		"code": "Visual Studio Code", "head": "output file start", "tail": "output file end",
		"cp": "copy files", "mv": "move files", "rm": "remove files",
		"touch": "update file timestamps", "wc": "count lines and words",
		"rg": "ripgrep search", "fd": "find entries",
		"jq": "JSON processor", "sed": "stream editor",
		"tar": "archive files", "unzip": "extract zip archives", "zip": "create zip archives",
		"sort": "sort lines", "uniq": "filter duplicate lines", "diff": "compare files",
		"stat": "file status", "file": "file type", "du": "disk usage",
		"curl": "transfer data from URLs", "wget": "download files",
	}
	for name, desc := range fileTools {
		out = append(out, &complete.Spec{Name: name, Description: desc, Icon: "file", Generator: files})
	}
	dirTools := map[string]string{
		"ls": "list directory contents", "eza": "modern ls", "tree": "directory tree",
		"mkdir": "create directories", "rmdir": "remove empty directories",
	}
	for name, desc := range dirTools {
		out = append(out, &complete.Spec{Name: name, Description: desc, Icon: "folder", Generator: dirs})
	}
	out = append(out,
		// egrep and gawk are aliases so those catalog entries resolve to
		// the same specs.
		&complete.Spec{Name: "grep", Aliases: []string{"egrep"}, Description: "search file contents", Icon: "file", Generator: files, Options: []complete.Option{
			{Names: []string{"-r", "-R", "--recursive"}, Description: "search directories recursively"},
			{Names: []string{"-i", "--ignore-case"}, Description: "case-insensitive matching"},
			{Names: []string{"-n", "--line-number"}, Description: "prefix matches with line numbers"},
			{Names: []string{"-l", "--files-with-matches"}, Description: "list matching file names only"},
			{Names: []string{"-v", "--invert-match"}, Description: "select non-matching lines"},
			{Names: []string{"-E", "--extended-regexp"}, Description: "extended regular expressions"},
			{Names: []string{"-c", "--count"}, Description: "count matching lines"},
		}},
		&complete.Spec{Name: "awk", Aliases: []string{"gawk"}, Description: "pattern scanning and processing", Icon: "file", Generator: files, Options: []complete.Option{
			{Names: []string{"-F"}, Description: "field separator", TakesArg: true},
			{Names: []string{"-v"}, Description: "assign a variable", TakesArg: true},
			{Names: []string{"-f"}, Description: "read the program from a file", TakesArg: true},
		}},
		&complete.Spec{Name: "z", Description: "zoxide jump", Icon: "folder", Generator: generators.Zoxide()},
		&complete.Spec{Name: "zi", Description: "zoxide interactive jump", Icon: "folder"},
		&complete.Spec{Name: "man", Description: "manual pages", Icon: "file"},
		&complete.Spec{Name: "which", Description: "locate a command", Icon: "file"},
		&complete.Spec{Name: "tmux", Description: "terminal multiplexer", Icon: "process", Subcommands: []*complete.Spec{
			{Name: "new", Description: "create a session"},
			{Name: "attach", Aliases: []string{"a"}, Description: "attach to a session"},
			{Name: "ls", Description: "list sessions"},
			{Name: "kill-session", Description: "kill a session"},
		}},
	)
	return out
}
