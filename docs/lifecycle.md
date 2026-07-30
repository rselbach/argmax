# Installation and lifecycle

## Installer and artifacts

The repository installer supports `stable` and `nightly` release channels and
the four platform assets listed in the [project README](../README.md). It
requires `curl` or `wget` plus `sha256sum` or `shasum`. Downloads use HTTPS in a
unique private temporary directory. The checksum is validated before the
existing executable can change, and the staged executable must report a valid
version for the selected channel.

Installer controls:

| Variable | Effect |
| --- | --- |
| `ARGMAX_INSTALL_DIR` | Requested absolute executable directory. |
| `ARGMAX_CHANNEL` | `stable` (default) or `nightly`. |
| `ARGMAX_VERSION` | Exact semantic version, with an optional leading `v`. It must match the channel. |

Examples:

```sh
ARGMAX_CHANNEL=nightly sh argmax-install.sh

ARGMAX_CHANNEL=stable \
ARGMAX_VERSION=0.1.0 \
ARGMAX_INSTALL_DIR="$HOME/.local/bin" \
sh argmax-install.sh
```

Re-running the installer replaces only the verified binary. It preserves config,
state, learning data, and shell hooks.

## Shell setup

`argmax setup [shell]` detects the active supported shell or accepts `bash`,
`zsh`, or `fish`. It reports the exact file it changed and adds one block between
stable argmax markers. Existing unrelated content and hooks remain in place.
Running setup again is a no-op. Before changing an existing file for the first
time, setup writes a sibling backup named like
`.bashrc.argmax-backup.UNIX_TIMESTAMP` and prints its path.

Default targets are:

| Shell | Configuration target |
| --- | --- |
| Bash | `$HOME/.bashrc` |
| Zsh | `$ZDOTDIR/.zshrc` when `ZDOTDIR` is set; otherwise `$HOME/.zshrc` |
| Fish | `$XDG_CONFIG_HOME/fish/config.fish` when nonempty and absolute; otherwise `$HOME/.config/fish/config.fish` |

A relative `ZDOTDIR` is resolved from the directory where setup runs. An empty
`ZDOTDIR` selects `/.zshrc`; setup fails safely if the resulting path cannot be
modified under its filesystem checks.

Setup does not source those files. If detection is unavailable, print and source
the integration manually:

```sh
# Bash
eval "$(argmax init bash)"

# Zsh
eval "$(argmax init zsh)"

# Fish
argmax init fish | source
```

`argmax init` writes sourceable code only to standard output. A new terminal is
the simplest way to activate a hook installed by `argmax setup`.

## Updates

With `updater.check-on-startup = true`, a session may check GitHub release
metadata in the background after startup. Stable ignores prereleases; nightly
uses the rolling nightly release. Network and parsing failures stay silent in
normal interactive use. A new-version notice is persisted and shown at most
once for that version, after a completed command.

No background check downloads or installs an executable. Apply an update
explicitly:

```sh
argmax update
```

The command selects the current OS/architecture asset, validates release
metadata and its SHA-256 record, downloads with a bounded deadline, and
atomically exchanges the current executable only after verification. Config,
state, learning data, and hooks remain untouched. An already-running wrapper
continues using its original executable; after success, exit that argmax session
naturally or open a new terminal as instructed.

### Roll back a binary

The supported deterministic rollback is a verified pinned install. Obtain the
installer associated with the release you trust, choose the directory containing
the active binary, and set the prior semantic version:

```sh
ARGMAX_CHANNEL=stable \
ARGMAX_VERSION=PREVIOUS_VERSION \
ARGMAX_INSTALL_DIR=/absolute/path/to/bin \
sh argmax-install.sh
```

The installer verifies that the downloaded binary reports exactly
`PREVIOUS_VERSION`. It does not downgrade config or persistent data. If the
newer binary migrated data to a schema the older binary does not understand,
exit all argmax sessions and restore the corresponding timestamped migration
backup first. Preserve the newer file separately so the rollback itself is
reversible.

## Migration and backups

Starting the interactive wrapper checks migration needs before entering raw
terminal mode. The implementation accepts schema-1 config, missing legacy schema
markers, hyphenated or underscore AI key spellings, and the legacy inactive
`ai.suggest_on_empty` field. It also discovers documented legacy argmax runtime
state (`state.toml`, `state.json`, and `update_state.json`) and compatible legacy
learning databases, including prior `iris` state/data locations.

Before rewriting, migration creates a private durable sibling backup. Config,
runtime state, and learning-database backups use names like:

```text
config.toml.backup.UNIX_TIMESTAMP
state.toml.backup.UNIX_TIMESTAMP
history.db.backup.UNIX_TIMESTAMP
```

Name collisions gain a numeric suffix and never overwrite an older backup.
Config and state publication is atomic; SQLite migration uses a validated
database snapshot. Config and runtime-state imports from legacy locations leave
their sources in place. A selected legacy learning database may instead be
migrated in place after its backup is validated. Re-running a completed
migration is a no-op.

A config migration failure prevents the interactive wrapper from starting and
prints an actionable error without replacing the only source. Corrupt runtime
state is preserved and defaults are used. State or learning migration failures
are reported as warnings and local completion continues where possible.

For a manual rollback, stop argmax sessions, identify the backup beside the
active file, copy the active file aside, then restore the chosen backup under its
original filename. Do not delete the backup until the older binary has started
successfully. Shell-hook backups use the separate
`.SHELLRC.argmax-backup.UNIX_TIMESTAMP` naming shown under shell setup.

## Crash reports and rescue shells

The interactive wrapper contains panics and abnormal runtime failures. Recovery
restores terminal modes and display state before diagnostics, writes a private
crash report, prints its absolute path to standard error, and starts the selected
shell as a rescue. If that shell cannot start, `/bin/sh` is the fallback. Nothing
is uploaded automatically.

```sh
argmax crash-log
argmax crash-log --clear
```

The first command prints the newest report path or `no crash reports`. The
second removes argmax-owned crash reports only and reports each removal or
failure. See [debugging privacy](privacy.md#debugging-privacy) before sharing a
report.

## Uninstall and data removal

Run:

```sh
argmax uninstall
```

This explicit command removes marked argmax blocks from all three supported
shell config targets, the currently running known argmax executable, and the
argmax-owned config, data, and cache trees. It also removes exact retained
`.argmax-update-<32 lowercase hex>.tmp` artifacts next to that executable while
leaving similarly named files untouched. Recognized legacy `iris` config,
state, learning, and cache roots used by migration are included. Those trees
contain config, mode/update state, learned command data, debug logs, and crash
reports. Shell history owned by Bash, Zsh, or Fish is not removed.

Each changed shell file is backed up first. Unmarked legacy integration lines
are retained and reported for manual review. Every removed location and every
failure is printed; a partial failure returns non-zero. When uninstall is run
inside an active argmax session, let that shell exit naturally after the command
rather than killing the wrapper.
