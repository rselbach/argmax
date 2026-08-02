# argmax

`argmax` is a native, terminal-resident command completion and prediction
tool. It wraps your interactive shell (Bash, Zsh, or Fish) in a lightweight
PTY and gives you an editor-style completion menu, inline ghost text,
searchable history, and context-aware ranking — without leaving your real
terminal. It works in local terminals, SSH sessions, tmux, and Linux virtual
terminals, with no account, no daemon, and no telemetry.

The core experience is local and offline. AI completion is optional, disabled
by default, and works with any OpenAI-compatible endpoint (remote providers
such as Groq, or local ones such as Ollama).

## Install

With Go 1.25+ installed:

```sh
go install github.com/rselbach/argmax/cmd/argmax@latest
argmax setup          # hooks your shell, creates config
```

Or build from source:

```sh
go build -ldflags "-X main.version=$(git describe --tags --always)" -o argmax ./cmd/argmax
./argmax setup        # installs to ~/.local/bin, hooks your shell, creates config
```

Other channels — GitHub release tarballs, Homebrew tap, AUR, Nix flake,
deb/rpm, and the checksum-verified install script — are documented in
`packaging/README.md`.

Restart your terminal (or `source` the file `setup` reports). Your shell now
starts inside an `argmax` session.

Other commands: `argmax init <bash|zsh|fish>` prints the shell integration,
`argmax config init|show` manages configuration, `argmax update` self-updates,
`argmax reload` hot-reloads, `argmax crash-log` shows diagnostics, and
`argmax uninstall` removes everything cleanly.

## Using it

- Type a command and suggestions appear below the prompt, with ghost text
  for the top candidate: `git che` → `git checkout` + ghost `ckout`.
- **Tab** inserts the highlighted suggestion (adds a space, except after `/`
  so you can keep traversing paths).
- **Right Arrow** accepts only the ghost-text suffix.
- **Up/Down** navigates the menu (on an empty buffer, opens recent history).
- **Enter** submits exactly what you typed — it never inserts a suggestion.
  (In history mode the buffer already contains the entry you previewed with
  Up/Down, so Enter runs that.)
- **Ctrl+R** toggles spec/history mode (persisted when `core.mode = "last"`).
- **Shift+Tab** turns the suggestion menu off/on for the session.
- **Esc** hides the menu until your next edit.
- `Ctrl+A/E/W/U/C/L` behave exactly like your shell.

Completion sources: the bundled catalog of 566 commands with nested
subcommands and flags, executables on `PATH`, shell aliases, Git/Cargo
aliases, files and directories, live values (branches, containers, images,
SSH hosts, package scripts, make/just targets, processes, env vars, installed
packages), Cobra `__complete` inference for unknown CLIs, and your shell
history with fuzzy, alias-aware search. Ranking adapts to your workspace and
your successful workflows via a local SQLite database (frecency + command
transitions) — all stored on your machine.

## Configuration

One commented TOML file (`$XDG_CONFIG_HOME/argmax/config.toml`, on macOS
`~/Library/Application Support/argmax/config.toml` when XDG is unset).
Everything has a compiled default; environment variables (`ARGMAX_*`) and CLI
flags override the file. The file is watched and most settings apply live.
Run `argmax config show` to see the fully resolved, redacted configuration.

AI is off by default. Enabling it means a bounded, documented context
snapshot (your buffer, cwd, recent commands, visible file names, workspace
signatures, bounded git state, and allowlisted `--help` output) may be sent
to the configured provider — see the disclosure block in the generated
config. No context is ever gathered or sent while AI is disabled.

## Notes and limitations

- **Reload:** `argmax reload` re-executes the session in place, retaining
  your launch arguments, the selected shell, and the working directory. State
  that lives inside the inner shell process — shell variables, jobs, traps —
  cannot survive replacement and is lost; the inner shell is restarted.
- The completion parser is intentionally shell-like, not a full POSIX parser.
- Fish integration is implemented and unit-tested against the fish 4.x event
  model; validate on your fish version with `argmax init fish | source`.
- No Windows/PowerShell/Nushell, no GUI, no cloud accounts, no telemetry.

## Development

```sh
make build      # version-injected build into ./bin
make test       # full test suite with -race
make vet        # go vet
make fmt        # gofmt check
```

Layout: `cmd/argmax` (entry point), `internal/session` (PTY wrapper, input,
watchdog), `internal/engine` (orchestrator), `internal/spec` (catalog +
resolver), `internal/sources` (PATH/aliases/files/live generators/Cobra),
`internal/history`, `internal/rank` (SQLite frecency/transitions + workspace
detection), `internal/overlay` (menu/ghost rendering), `internal/shell`
(init scripts + hooks), `internal/ai`, `internal/config`, `internal/updater`,
`internal/cli`, `internal/logs`, `internal/core`.
