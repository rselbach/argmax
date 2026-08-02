package config

// DefaultFile returns the fully commented default configuration written by
// `argmax config init` and `argmax setup` (PRD CFG-002, 11.2, DIAG-007).
const DefaultFile = `# argmax configuration
#
# Every setting below shows its compiled default. Uncomment and edit to
# customize. Precedence (lowest to highest): compiled defaults, this file,
# supported environment variables, explicit CLI flags.

[core]
# Configuration schema version. Do not edit; migrations key off it.
# version = 1
# Shell to wrap: "", "bash", "zsh", or "fish". Empty means auto-detect.
# shell = ""
# Start the shell as a login shell.
# shell-login = false
# Startup suggestion mode: "last", "spec", or "history".
# mode = "last"
# Enable debug diagnostics.
#
# PRIVACY WARNING: debug logs may contain everything you type, including
# commands and queries. Logs stay on this machine; share them with care.
# debug = false
# Typing a space after a single shell alias expands it in place.
# expand-alias = true

[ui]
# Menu style: "modern" or "classic".
# style = "modern"
# Show inline ghost text for the top suggestion.
# ghost-text = true
# Show dot-prefixed files in file completion.
# hidden-files = false
# Maximum number of merged suggestions (1-500).
# max-suggestions = 100
# Maximum menu height in rows (3-50).
# max-height = 15
# Maximum menu width in columns. 0 means responsive with a preferred
# width of 76 columns; the terminal width remains the hard ceiling.
# max-width = 0
# Use Nerd Font glyphs for icons. Set false for plain terminals.
# nerd-fonts = true

[keybindings]
# Key names: a single character, "ctrl+<letter>", "ctrl+space", "tab",
# "shift+tab", "up", "down", "left", "right", "enter"/"return"/"cr".
# Case-insensitive; "-" is accepted as an alias for "+".
# "ctrl+m" is reserved and always submits the command.
# toggle-mode = "ctrl+r"
# toggle-menu = "shift+tab"
# select = "tab"
# navigate-up = "up"
# navigate-down = "down"

[git]
# Omit the currently checked-out branch from checkout/switch suggestions.
# filter-active-branch = true
# Merge local branches with equivalent remote-tracking names.
# deduplicate-branches = true

[updater]
# Check for updates asynchronously after session startup. Never blocks the
# first prompt. Independent of AI/network policy.
# check-on-startup = true
# Release channel: "stable" or "nightly".
# channel = "stable"
# Minimum interval between checks, as a Go duration (e.g. "30m", "6h", "24h").
# check-interval = "24h"

# ---------------------------------------------------------------------------
# AI completion (OPTIONAL, disabled by default)
#
# When ai.enabled = true, argmax may send a bounded context snapshot to the
# configured provider. A request may contain:
#   - the current command buffer and working-directory path
#   - recent commands and the previous exit status
#   - visible file/directory names
#   - workspace signature names and package/build scripts
#   - Git branch names, short status, staged diff, and recent commit subjects
#   - selected live resource names and bounded --help output
# No environment-variable values, file contents, unstaged diffs, or
# credentials are sent. While AI is disabled, no endpoint is contacted and no
# context is gathered for transmission. A local provider such as Ollama
# follows the same request contract without cloud disclosure.
#
# Example (Groq):
# [ai]
# enabled = true
# provider = "groq"
# debounce_ms = 500        # debounce typed requests; 0 selects the fallback
# min_interval_ms = 1000   # minimum interval between calls
#
# [ai.providers.groq]
# inherited_from = "openai"
# endpoint = "https://api.groq.com/openai/v1"
# api_key_env = "GROQ_API_KEY"   # preferred over a direct api_key
# model = "llama-3.3-70b-versatile"
# timeout_ms = 3000
#
# [ai.providers.groq.extra_request_body]
# temperature = 0.2
# max_tokens = 100
#
# Example (local Ollama):
# [ai.providers.ollama]
# inherited_from = "openai"
# endpoint = "http://localhost:11434/v1"
# model = "qwen2.5-coder"
# timeout_ms = 5000
#
# [ai.suggest_on_empty]
# enabled = false          # empty-prompt prediction, independently controlled
# debounce_ms = 800
# min_interval_ms = 5000
# ---------------------------------------------------------------------------

[ai]
# enabled = false
# provider = ""
# debounce_ms = 500
# min_interval_ms = 1000

[ai.suggest_on_empty]
# enabled = false
# debounce_ms = 800
# min_interval_ms = 5000
`
