# Configuration

`argmax setup` creates the commented default config only when none exists.
`argmax config init` performs the same config-only initialization and prints the
exact path. On Linux the normal location is
`${XDG_CONFIG_HOME:-$HOME/.config}/argmax/config.toml`; other supported systems
use their standard per-user config directory. An absolute `XDG_CONFIG_HOME` is
honored.

Inspect the effective settings without exposing configured credentials:

```sh
argmax config show
```

Resolution order is an interactive CLI flag, a supported environment override,
the TOML file, then the compiled default. Environment and CLI values are not
written back to the file.

## Schema and defaults

The current schema is version 2. `argmax config init` writes these values:

| Key | Default | Meaning |
| --- | --- | --- |
| `core.version` | `2` | Managed config schema. |
| `core.shell` | `""` | Automatic selection, or `bash`, `zsh`, or `fish`. |
| `core.mode` | `"last"` | Restore the last selection, or force `spec`/`history` at session start. |
| `core.debug` | `false` | Private diagnostic logging. |
| `core.expand-alias` | `true` | Expand an exact root alias on a typed space. |
| `ui.style` | `"modern"` | `modern` or `classic`. |
| `ui.nerd-fonts` | `true` | Permit Nerd Font icon glyphs. |
| `ui.hidden-files` | `false` | Include dot-prefixed filesystem results. |
| `ui.ghost-text` | `true` | Show the selected candidate's suffix. |
| `ui.max-suggestions` | `100` | Ranked result limit, from 1 through 500. |
| `ui.max-height` | `15` | Visible rows, from 3 through 50. |
| `keybindings.toggle-mode` | `"ctrl+r"` | Mode-toggle binding. |
| `keybindings.toggle-menu` | `"shift+tab"` | Session-menu binding. |
| `git.filter-active-branch` | `true` | Hide the active branch when choosing it is a no-op. |
| `git.deduplicate-branches` | `true` | Merge equivalent local and remote branch rows. |
| `updater.check-on-startup` | `true` | Permit a background GitHub release check. |
| `updater.channel` | `"stable"` | `stable` or `nightly`. |
| `updater.check-interval` | `"24h"` | Interval from one minute through 30 days. |
| `ai.enabled` | `false` | Permit requests to the selected AI provider. |
| `ai.provider` | `""` | Selected entry in `ai.providers`. |
| `ai.context-level` | `"minimal"` | `minimal`, `workspace`, or `full`. |
| `ai.debounce_ms` | `500` | Delay after a buffer change, from 0 through 10000 ms. |
| `ai.min_interval_ms` | `1000` | Minimum provider-call spacing, from 1 through 60000 ms. |

A named `[ai.providers.NAME]` table accepts:

| Key | Default | Meaning |
| --- | --- | --- |
| `inherited_from` | `"openai"` | OpenAI-compatible chat-completion protocol. |
| `endpoint` | none | HTTPS base/completion URL, or loopback HTTP URL. |
| `api_key_env` | none | Preferred environment variable containing a credential. |
| `api_key` | none | Compatibility plaintext credential; discouraged and redacted. |
| `model` | none | Provider model identifier. |
| `timeout_ms` | `2000` | Request timeout, from 1 through 60000 ms. |
| `extra_request_body` | empty | Provider fields that do not override enforced safety fields. |

Unknown keys produce warnings. Invalid known values reject the complete
candidate config. If enabled AI does not name an existing table with a nonblank
endpoint and model, AI requests remain unavailable and `argmax config show` or
debug logging reports the missing field; local completion continues normally.
When `api_key_env` is configured, that environment variable or the compatibility
`api_key` fallback must contain the credential. Providers that do not require
authentication may omit both fields.

## Environment overrides

| Environment variable | Setting |
| --- | --- |
| `argmax_CORE_DEBUG` | `core.debug` |
| `argmax_CORE_SHELL` | `core.shell` |
| `argmax_CORE_MODE` | `core.mode` |
| `argmax_UI_GHOST_TEXT` | `ui.ghost-text` |
| `argmax_UI_MAX_SUGGESTIONS` | `ui.max-suggestions` |
| `argmax_UI_MAX_HEIGHT` | `ui.max-height` |
| `argmax_UPDATER_CHANNEL` | `updater.channel` |
| `argmax_UPDATER_INTERVAL` | `updater.check-interval` |
| `argmax_UPDATER_CHECK_ON_STARTUP` | `updater.check-on-startup` |
| `argmax_AI_ENABLED` | `ai.enabled` |
| `argmax_AI_PROVIDER` | `ai.provider` |

`argmax_LOG_LEVEL` enables diagnostics at `trace`, `debug`, `info`, `warn`, or
`error` without changing resolved TOML. Invalid override values are errors.

## Live reload

An interactive session checks the config at most once per second. A complete,
valid replacement updates UI, keybindings, Git filtering, alias behavior,
updater scheduling, and AI settings. Disabling AI or changing its selected
provider cancels prior AI authority immediately. A shell, initial mode, or debug
logging change takes effect when the next interactive wrapper starts.

Malformed, missing-after-startup, or invalid files do not replace the
last-known-good generation. Run `argmax config show` to obtain the field-level
error. From inside an active session, this bypasses the polling delay and waits
for an acknowledgement:

```sh
argmax reload
```

Reload waits until an in-progress configurable key prefix is safe to replace, so
typing retains priority over a config edit.
