package config

// DefaultTemplate is the fully commented configuration written by
// `argmax config init` when no configuration exists.
const DefaultTemplate = `# argmax configuration
# Values shown are the compiled defaults. Uncomment and edit to override.
# Reloadable settings apply live; shell selection applies on the next session.

[core]
# Configuration schema version. Do not edit.
version = 1
# Shell to wrap: "", "bash", "zsh", or "fish". Empty auto-detects.
#shell = ""
# Start the shell as a login shell.
#shell-login = false
# Startup suggestion mode: "last", "spec", or "history".
#mode = "last"
# Verbose diagnostics. WARNING: debug logs may contain everything you type.
#debug = false
# Replace a typed alias with its expansion when you press space.
#expand-alias = true
# Allow Enter to submit the highlighted candidate without prior navigation.
#auto-execute = false

[ui]
# Menu style: "modern" or "classic".
#style = "modern"
# Show the inline ghost-text suffix of the selected candidate.
#ghost-text = true
# Include dot-prefixed files in path completion.
#hidden-files = false
# Maximum merged candidates (1-500).
#max-suggestions = 100
# Maximum menu height in rows (3-50).
#max-height = 15
# Maximum menu width in columns; 0 means responsive (preferred 76).
#max-width = 0
# Use Nerd Font glyphs for icons.
#nerd-fonts = true

[keybindings]
# Accepted: a single character, "ctrl+<letter>", "ctrl+space", "tab",
# "shift+tab", "up", "down", "left", "right". "ctrl+m"/"enter" is reserved.
#toggle-mode = "ctrl+r"
#toggle-menu = "shift+tab"
#select = "tab"
#navigate-up = "up"
#navigate-down = "down"

[git]
# Omit the currently checked-out branch from checkout/switch suggestions.
#filter-active-branch = true
# Merge local branches with their equivalent remote-tracking names.
#deduplicate-branches = true

[updater]
# Check for new releases asynchronously after startup.
#check-on-startup = true
# Release channel: "stable" or "nightly".
#channel = "stable"
# Minimum interval between checks (Go duration).
#check-interval = "24h"

[ai]
# AI completion is disabled by default. While disabled, no AI endpoint is
# contacted and no context is gathered for transmission.
#
# When a cloud provider is enabled, a request may contain: the current
# command buffer and working-directory path; recent commands and previous
# exit status; visible file and directory names; workspace signature names
# and package/build scripts; Git branch names, short status, staged diff,
# and recent commit subjects; selected live resource names; and bounded
# --help output. Environment-variable values, file contents, unstaged
# diffs, and credentials are never sent.
#enabled = false
# Name of the active provider configured below.
#provider = ""
# Delay after typing before an AI request (milliseconds).
#debounce_ms = 500
# Minimum interval between AI requests (milliseconds).
#min_interval_ms = 1000

[ai.suggest_on_empty]
# Predict a command on an empty prompt. Local rules run before AI.
#enabled = false
#debounce_ms = 800
#min_interval_ms = 5000

# Example providers. api_key_env is the recommended credential mechanism.
#
#[ai.providers.groq]
#inherited_from = "openai"
#endpoint = "https://api.groq.com/openai/v1"
#api_key_env = "GROQ_API_KEY"
#model = "llama-3.3-70b-versatile"
#timeout_ms = 3000
#
#[ai.providers.groq.extra_request_body]
#temperature = 0.2
#max_tokens = 100
#
#[ai.providers.ollama]
#inherited_from = "openai"
#endpoint = "http://localhost:11434/v1"
#model = "qwen2.5-coder"
#timeout_ms = 5000
`
