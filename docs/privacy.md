# AI, privacy, and security

argmax has no analytics, advertising identifiers, account identifiers, usage
events, or command telemetry. Local completion works with both AI and update
checks disabled.

argmax's built-in remote operations are:

- GitHub release metadata when `updater.check-on-startup = true` (the default),
  no more often than `updater.check-interval`;
- release metadata, checksum, and executable downloads for an installer run or
  explicit `argmax update`; and
- the selected AI endpoint only when `ai.enabled = true` and the provider is
  complete and valid.

Set `updater.check-on-startup = false` and leave `ai.enabled = false` to disable
those two session-time remote clients. Local completion remains available.

## Optional AI completion

AI is disabled by default. Enabling it authorizes sending the exact current
input buffer to the configured endpoint. A shell buffer can contain passwords,
tokens, private hostnames, paths, or other sensitive values; review it before
enabling a remote provider.

Configure an OpenAI-compatible provider explicitly:

```toml
[ai]
enabled = true
provider = "openai"
context-level = "minimal"
debounce_ms = 500
min_interval_ms = 1000

[ai.providers.openai]
inherited_from = "openai"
endpoint = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
model = "your-model"
timeout_ms = 2000
```

Then supply the credential to the argmax process without putting its value in
TOML:

```sh
export OPENAI_API_KEY='your-provider-key'
```

Provider base URLs receive `/chat/completions`; a URL already ending in that
path is retained. HTTPS is required for remote hosts. Plain HTTP is accepted
only for canonical loopback endpoints such as an Ollama-compatible
`http://127.0.0.1:11434/v1` endpoint. Redirects are not followed.

`argmax config show` displays the selected provider, context level, and a
credential-safe endpoint. Credential values, endpoint query strings, inline
compatibility keys, and secret-like extra fields are redacted.

### Context levels

All levels include the exact input buffer and shell/OS metadata:

| Level | Additional eligible data |
| --- | --- |
| `minimal` | Nothing from the repository or current directory. |
| `workspace` | Current directory path, up to three recent commands, detected project marker filenames, immediate directory names, package scripts, Make/Just targets, bounded allowlisted command help, and relevant local resource names. |
| `full` | Workspace data plus bounded Git status, staged diff, branch names, and recent commit subjects. |

Changing from `minimal` to a broader level is a separate config action. argmax
does not read arbitrary source-file contents for AI context. Workspace mode
reads bounded structured project metadata; full mode additionally permits the
bounded staged diff listed above.

Requests require at least three non-whitespace characters and are debounced and
rate-spaced. Buffer changes, cursor movement away from the end, mode changes,
menu navigation, command execution, provider changes, disablement, and session
exit invalidate earlier work. A response is accepted only when it is one inert,
control-free line beginning with the current buffer exactly. It is never
executed automatically.

## Local data

argmax stores settings in the per-user config area, runtime/update state and a
SQLite learning database in the per-user data area, and diagnostics in the
per-user cache area. Files and owned directories are created with user-only
write access; sensitive stores are private to the user.

The learning database retains successful/failed command observations and
working-directory-aware ranking data until uninstall. Shell history files stay
owned by the shell; argmax reads them for completion but does not remove them.
Transient parsed history and provider context caches live in session memory.

Crash reports and debug logs are never uploaded automatically.

## Debugging privacy

Debug logging is off by default. Enable it for one wrapper with `argmax --debug`,
through `core.debug`, or with `argmax_LOG_LEVEL`. Startup prints this warning:

> argmax debug logging is enabled; typed commands may contain secrets

The active log is in the platform user-cache directory under
`argmax/logs/debug.log`. It rotates at five MiB and retains one
`debug.previous.log`. Normal mode does not log every key or command. Diagnostic
messages redact known authorization headers, bearer values, URL credentials,
and common secret/token/API-key forms, but redaction cannot guarantee that an
arbitrary command contains no private material. Review logs before sharing.

Crash reports apply the same credential-pattern redaction, are stored privately
under the user cache, and can contain failure and stack information. Locate the
newest report with `argmax crash-log`; delete all argmax crash reports with
`argmax crash-log --clear`.

## Local provider safety

Rendered suggestions are sanitized and remain inert. Dynamic providers and Git
context probes use direct executable/argument arrays rather than a shell, with
bounded execution time and output. A displayed suggestion is never executed to
validate it. Installed executables invoked for their completion protocols keep
their own network and credential behavior; argmax bounds the process but does
not provide an operating-system network sandbox for it.
