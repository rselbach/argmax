# argmax

argmax is a local, terminal-native command assistant for Bash, Zsh, and Fish. It
runs the selected shell inside a PTY, forwards input immediately, and renders
inert command suggestions from its built-in catalog and local context. Optional
AI completion is disabled by default.

Release binaries are published for Linux and macOS on 64-bit x86 and Arm:

| Platform | Release asset |
| --- | --- |
| Linux x86_64 | `argmax-linux-amd64` |
| Linux aarch64 | `argmax-linux-arm64` |
| macOS x86_64 | `argmax-macos-amd64` |
| macOS arm64 | `argmax-macos-arm64` |

An interactive session requires an installed Bash, Zsh, or Fish executable, a
TTY, and a terminal supporting common ANSI/VT sequences. For SSH sessions,
install argmax on the remote host.

## Install

Install the latest stable release with one command:

```sh
curl -fsSL https://raw.githubusercontent.com/rselbach/argmax/main/scripts/install.sh \
  | sh
```

The installer selects the matching asset, downloads its `.sha256` file, verifies
the binary, validates the version reported by the executable, and publishes it
atomically. It uses `/usr/local/bin` when that directory is writable and safe;
otherwise it uses `$HOME/.local/bin`. Follow the exact PATH and `argmax setup`
commands it prints, then open a new terminal.

To inspect the installer before running it:

```sh
curl -fsSL -o argmax-install.sh \
  https://raw.githubusercontent.com/rselbach/argmax/main/scripts/install.sh
less argmax-install.sh
sh argmax-install.sh
rm argmax-install.sh
```

To select an absolute directory instead:

```sh
ARGMAX_INSTALL_DIR="$HOME/.local/bin" sh argmax-install.sh
```

The installer does not edit shell configuration itself.

Then install one marked shell hook and create the default config if it is absent:

```sh
argmax setup
```

Pass `bash`, `zsh`, or `fish` when automatic detection is not appropriate:

```sh
argmax setup zsh
```

Open a new terminal, or source the generated integration manually as described
in the [lifecycle guide](docs/lifecycle.md#shell-setup).

### Verify a release artifact manually

Download the platform asset and its identically named `.sha256` file into the
same directory. On Linux:

```sh
sha256sum --check argmax-linux-amd64.sha256
chmod 0755 argmax-linux-amd64
./argmax-linux-amd64 version
```

On macOS:

```sh
shasum -a 256 --check argmax-macos-arm64.sha256
chmod 0755 argmax-macos-arm64
./argmax-macos-arm64 version
```

Use the asset matching the host table above. Confirm that the printed semantic
version matches the release before moving the executable to a directory on
`PATH` under the name `argmax`.

## Use

Run `argmax` in an interactive terminal. The defaults are:

- `Ctrl+R`: toggle specification and history modes.
- `Shift+Tab`: toggle the session menu.
- `Up`/`Down`: move through suggestions; on an empty prompt they can enter
  history navigation.
- `Tab`: insert the selected suggestion without executing it.
- `Right`: accept visible ghost text.
- `Escape`: dismiss transient argmax UI.
- `Enter`: execute only the current shell buffer.

The full built-in inventory is in the [generated command catalog](docs/commands.md).

## CLI

| Command | Effect |
| --- | --- |
| `argmax` | Start an interactive wrapped shell. |
| `argmax --shell <bash\|zsh\|fish>` | Override the shell for this session. |
| `argmax --debug` | Start with private diagnostic logging. |
| `argmax init <bash\|zsh\|fish>` | Print sourceable integration code to standard output. |
| `argmax setup [bash\|zsh\|fish]` | Install one idempotent shell hook and initialize config. |
| `argmax config init` | Create the commented default config if absent. |
| `argmax config show` | Print fully resolved, credential-redacted settings. |
| `argmax reload` | Reload configuration in the active argmax session. |
| `argmax version` | Print the running semantic version. |
| `argmax update` | Check, verify, and atomically install an available release. |
| `argmax crash-log` | Print the newest private crash-report path. |
| `argmax crash-log --clear` | Remove argmax crash reports. |
| `argmax uninstall` | Remove managed hooks, the running binary, and argmax local data. |

`--shell` and `--debug` apply only when starting an interactive session.
Subcommand errors go to standard error and return a non-zero status.

## Documentation

- [User guide](docs/README.md)
- [Configuration and live reload](docs/configuration.md)
- [AI, privacy, and security](docs/privacy.md)
- [Install, update, migration, recovery, rollback, and removal](docs/lifecycle.md)
- [Built-in command catalog](docs/commands.md)

## Build from source

argmax requires Rust 1.85 or newer.

```sh
cargo build --locked --release
./target/release/argmax version
```

Building from source creates the executable only; run `argmax setup` after
placing it on `PATH`.
