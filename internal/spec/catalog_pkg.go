package spec

// pkgCount is the PRD 18.9 category size (system package managers).
const pkgCount = 12

func catalogPkg() []*Spec {
	return []*Spec{
		aptSpec("apt", "Debian package manager"),
		aptSpec("apt-get", "Debian package handling utility"),
		brewSpec(),
		dnfSpec("dnf", "Fedora package manager"),
		{
			Name:        "dpkg",
			Description: "Debian package manager (low level)",
			Icon:        "package",
			Options: []Option{
				optGen("files", "Install a .deb package", "-i", "--install"),
				optD("List installed packages", "-l", "--list"),
				optVal("List files owned by a package", "-L", "--listfiles"),
				optVal("Show package status", "-s", "--status"),
				optVal("Remove a package", "-r", "--remove"),
				optVal("Purge a package", "-P", "--purge"),
			},
		},
		{
			Name:        "flatpak",
			Description: "Linux application sandboxing and distribution",
			Icon:        "package",
			Subcommands: []*Spec{
				cmd("install", "Install an application", "package"),
				cmd("uninstall", "Uninstall an application", "package"),
				cmd("list", "List installed applications", "package"),
				cmd("search", "Search for applications", "package"),
				cmd("info", "Show application details", "package"),
				cmd("run", "Run an application", "package"),
				cmd("update", "Update applications", "package"),
			},
		},
		pacmanSpec("pacman", "Arch Linux package manager"),
		pacmanSpec("paru", "AUR helper (paru)"),
		{
			Name:        "pkgutil",
			Description: "macOS package installer receipts tool",
			Icon:        "package",
			Options: []Option{
				optD("List installed packages", "--pkgs"),
				optVal("List files installed by a package", "--files"),
				optVal("Show package metadata", "--pkg-info"),
			},
		},
		{
			Name:        "snap",
			Description: "Ubuntu snap package manager",
			Icon:        "package",
			Subcommands: []*Spec{
				cmd("install", "Install a snap", "package"),
				cmd("remove", "Remove a snap", "package"),
				cmd("list", "List installed snaps", "package"),
				cmd("find", "Search for snaps", "package"),
				cmd("info", "Show snap details", "package"),
				cmd("refresh", "Update snaps", "package"),
			},
		},
		pacmanSpec("yay", "AUR helper (yay)"),
		dnfSpec("yum", "Yellowdog Updater package manager"),
	}
}

// brewSpec builds the Homebrew spec; uninstall/reinstall complete installed
// packages.
func brewSpec() *Spec {
	caskFlags := []Option{
		optD("Operate on casks", "--cask"),
		optD("Operate on formulae", "--formula"),
	}
	return &Spec{
		Name:        "brew",
		Description: "Homebrew package manager",
		Icon:        "package",
		Subcommands: []*Spec{
			{
				Name: "install", Description: "Install a formula or cask", Icon: "package",
				Options: append(append([]Option{}, caskFlags...),
					optD("Build from source", "-s", "--build-from-source")),
			},
			{Name: "uninstall", Aliases: []string{"remove", "rm"}, Description: "Uninstall a formula or cask", Icon: "package", Options: caskFlags, Generator: "packages-installed"},
			{Name: "reinstall", Description: "Reinstall a formula or cask", Icon: "package", Options: caskFlags, Generator: "packages-installed"},
			{Name: "list", Description: "List installed formulae and casks", Icon: "package", Options: caskFlags},
			{Name: "search", Description: "Search formulae and casks", Icon: "package", Options: caskFlags},
			{Name: "info", Description: "Show formula or cask details", Icon: "package", Options: caskFlags},
			cmd("update", "Update Homebrew and its taps", "package"),
			{
				Name: "upgrade", Description: "Upgrade installed formulae and casks", Icon: "package",
				Options: append(append([]Option{}, caskFlags...),
					optD("Upgrade everything", "--all")),
			},
			{
				Name: "services", Description: "Manage background services", Icon: "package",
				Subcommands: []*Spec{
					cmd("start", "Start a service", "package"),
					cmd("stop", "Stop a service", "package"),
					cmd("restart", "Restart a service", "package"),
					cmd("list", "List services", "package"),
					cmd("run", "Run a service without registering", "package"),
				},
			},
			cmd("tap", "Tap a third-party repository", "package"),
			{Name: "untap", Description: "Remove a tapped repository", Icon: "package"},
		},
	}
}

// aptSpec builds the apt/apt-get spec; remove/purge complete installed
// packages.
func aptSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "package",
		Subcommands: []*Spec{
			{
				Name: "install", Description: "Install packages", Icon: "package",
				Options: []Option{
					optD("Assume yes to prompts", "-y", "--yes"),
					optD("Do not install recommended packages", "--no-install-recommends"),
				},
			},
			{Name: "remove", Description: "Remove packages", Icon: "package", Generator: "packages-installed"},
			{Name: "purge", Description: "Remove packages and configuration", Icon: "package", Generator: "packages-installed"},
			cmd("update", "Update the package lists", "package"),
			cmd("upgrade", "Upgrade installed packages", "package"),
			cmd("search", "Search for packages", "package"),
			cmd("show", "Show package details", "package"),
			cmd("autoremove", "Remove unneeded packages", "package"),
		},
	}
}

// dnfSpec builds the dnf/yum spec; remove completes installed packages.
func dnfSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "package",
		Subcommands: []*Spec{
			{
				Name: "install", Description: "Install packages", Icon: "package",
				Options: []Option{
					optD("Assume yes to prompts", "-y", "--assumeyes"),
				},
			},
			{Name: "remove", Aliases: []string{"erase"}, Description: "Remove packages", Icon: "package", Generator: "packages-installed"},
			{Name: "update", Aliases: []string{"upgrade"}, Description: "Update packages", Icon: "package"},
			cmd("search", "Search for packages", "package"),
			cmd("list", "List packages", "package"),
			cmd("info", "Show package details", "package"),
		},
	}
}

// pacmanSpec builds the pacman/yay/paru spec. Removal-style operations
// complete installed packages; pacman expresses operations as flags, so the
// generator hangs off the root.
func pacmanSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "package",
		Options: []Option{
			optD("Sync (install) packages", "-S", "--sync"),
			optD("Remove packages", "-R", "--remove"),
			optD("Query the package database", "-Q", "--query"),
			optGen("files", "Install from a package file", "-U", "--upgrade"),
			optD("Full system upgrade", "-Syu"),
			optD("Force refresh of databases", "-Syy"),
			optVal("Search the sync databases", "-Ss"),
			optVal("Search installed packages", "-Qs"),
			optD("Remove with unneeded dependencies", "-Rns"),
			optD("Do not ask for confirmation", "--noconfirm"),
		},
		Generator: "packages-installed",
	}
}
