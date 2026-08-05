# argmax

Editor-style command completion for your real terminal.

`argmax` wraps your interactive shell — Bash, Zsh, or Fish — in a lightweight
PTY session and gives it an inline completion menu, ghost-text suggestions,
fuzzy history search, and context-aware ranking. Suggestions appear while you
type; nothing runs until you press Enter. It works in local terminals, SSH,
tmux, and Linux virtual terminals, with no account, no daemon, and no
telemetry.

```
% git che
    ❯  checkout    switch branches or restore files
       cherry-pick apply existing commits
    tab insert · ctrl+r mode · shift+tab hide
```

- **One ranked surface** — commands, subcommands, flags, files, shell and
  Git/Cargo aliases, history, and live values (branches, containers, SSH
  hosts, package scripts, processes) in a single menu.
- **567-command catalog** — bundled specifications with nested subcommands
  and options, plus automatic [Cobra](https://cobra.dev) `__complete`
  inference for CLIs it doesn't know.
- **Learns your workflow** — successful commands earn frecency in a local
  SQLite database, command sequences learn transitions (`git add` →
  `git commit`), and workspace signatures (go.mod, package.json, …) boost
  what's relevant to the repository you're in.
- **Local by default** — completion, history, ranking, and learning all work
  offline. AI completion exists but is off until you configure a provider,
  local (Ollama) or remote (any OpenAI-compatible endpoint).

## Install

From a published release:

```sh
# verified install script (checksum-checked)
curl -fsSL https://raw.githubusercontent.com/rselbach/argmax/main/scripts/install.sh | bash

# Homebrew
brew install rselbach/tap/argmax

# Go toolchain
go install github.com/rselbach/argmax/cmd/argmax@latest
```

`.deb` and `.rpm` packages ship with each release. The AUR PKGBUILD and Nix
flake are release-maintainer templates, not ready-to-use install channels;
see `packaging/README.md`. From source:

```sh
git clone https://github.com/rselbach/argmax && cd argmax
go build -o argmax ./cmd/argmax   # Go 1.26+
```

## Quick start

```sh
argmax setup
```

Setup detects your shell, installs a clearly marked autostart block in its
configuration file, and creates a commented config. Restart the terminal (or
source the reported file) and your shell now runs inside an argmax session.

You can also try it without installing anything: running `argmax` directly
wraps a shell for just that session (with slightly reduced accuracy until
the shell hooks are installed).

## Using it

| Key | Action |
| --- | --- |
| type | suggestions appear; ghost text shows the best completion's suffix |
| `Tab` | insert the highlighted suggestion |
| `→` (at end of line) | accept just the ghost suffix |
| `↑` / `↓` | navigate the menu; on an empty prompt, open recent history |
| `Ctrl+R` | toggle between spec and history mode |
| `Shift+Tab` | hide/show suggestions for this session |
| `Esc` | dismiss the menu until you edit again |
| `Enter` | run the line (or a deliberately selected suggestion) |

History mode searches your shell history plus everything typed this session,
with exact, prefix, substring, and fuzzy tiers — and it finds commands by
alias or expansion interchangeably (`gco` matches `git checkout` history).

## Configuration

Config lives at `~/.config/argmax/config.toml` (XDG-aware), fully commented
by `argmax config init`. Most settings — style, limits, keybindings, AI —
apply live when the file changes; `argmax config show` prints the resolved
values. Keybindings accept `ctrl+<letter>`, `tab`, `shift+tab`, arrows, or a
single character.

### AI completion (opt-in)

```toml
[ai]
enabled = true
provider = "ollama"

[ai.providers.ollama]
inherited_from = "openai"
endpoint = "http://localhost:11434/v1"
model = "qwen2.5-coder"
```

Any OpenAI-compatible endpoint works; `api_key_env` is the recommended
credential mechanism for cloud providers. Requests are debounced,
rate-limited, and canceled the moment you keep typing. A suggestion is never
executed automatically. Before enabling a cloud provider, know what a
request may contain: your current command line and working directory, recent
commands and exit status, visible file names, workspace signatures, Git
branch names/short status/staged diff, and bounded `--help` output — never
environment-variable values, file contents, unstaged diffs, or credentials.

## Privacy

Everything argmax learns stays on your machine: `~/.local/share/argmax`
holds the ranking database and state (mode 0600/0700), and caches are
disposable. There is no telemetry, no account, and no network traffic except
the release check (configurable in `[updater]`) and AI when you enable it.
`argmax uninstall` removes hooks, state, and binaries.

## Development

```sh
just check          # fmt + lint + race tests
just bench          # performance benchmarks
just docs           # regenerate docs/commands.md from the registry
```

The bundled catalog is generated from the MIT-licensed
[Fig autocomplete](https://github.com/withfig/autocomplete) corpus by
`tools/figexport` and `tools/cataloggen`; see `docs/commands.md` for the full
command list and `packaging/README.md` for release channels.

## License

MIT — see [LICENSE](LICENSE). Bundled completion data derived from Fig
autocomplete is used under its MIT license; see [NOTICE](NOTICE).
