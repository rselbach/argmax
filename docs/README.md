# User guide

argmax wraps one interactive Bash, Zsh, or Fish process in a PTY. Shell input is
forwarded immediately; completion, ranking, local process probes, persistence,
update checks, and optional AI work run outside the input path. Suggestions are
display text until the user explicitly inserts them, and insertion never presses
Enter.

## Typical workflow

1. Install the verified binary and run `argmax setup`.
2. Open a new terminal and type normally.
3. Use `Up` and `Down` to select a result, `Tab` to insert it, or `Right` to
   accept ghost text.
4. Press `Ctrl+R` to switch between specification and history modes.
5. Press `Escape` to dismiss argmax UI and continue with the shell unchanged.

The default `Shift+Tab` binding opens or closes the session menu. Both
configurable bindings are listed in the footer and can be changed in TOML.
Regular shell editing, bracketed paste, full-screen programs, and unsupported
terminal input continue through to the shell.

## Suggestion sources

Local completion combines the compiled [command catalog](commands.md), commands
on `PATH`, filesystem entries, aliases, shell history, current-session learning,
workspace metadata, and bounded dynamic providers. The local worker ranks its
complete candidate set before applying the display bound. Individual provider
failures degrade the result set rather than blocking typing.

AI is a separate, optional provider. Locally ranked suggestions stay ahead of
AI-only additions, even when the AI result reports higher confidence. Read the
[AI and privacy disclosure](privacy.md#optional-ai-completion) before enabling
it.

## Operations

- [Configuration and live reload](configuration.md)
- [Installation and lifecycle](lifecycle.md)
- [Privacy, network activity, and diagnostics](privacy.md)

Use `argmax --help` for the current command synopsis and `argmax config show`
for the effective, redacted settings.
