package shell

// InitScript returns the sourceable dual-mode integration script for s
// (SH-001). `argmax init <shell>` prints it to stdout; it never modifies
// files. The script has two branches:
//
//   - Inside a live argmax session (ARGMAX_SESSION set and fresh): install
//     hooks that stream NUL-delimited event records to the inherited hook
//     fd (SH-003, SH-004, SH-005, SH-006).
//   - Outside a session: autostart argmax, but only in an interactive,
//     non-nested, non-rescue shell (SH-002, RUN-010).
//
// The scripts never print to stdout or stderr in normal operation.
func (s Shell) InitScript() string {
	switch s {
	case Bash:
		return bashInitScript
	case Zsh:
		return zshInitScript
	case Fish:
		return fishInitScript
	}
	return ""
}

// The header comment of each script documents the managed markers by
// quoting them; it must never contain a line that IS exactly BlockBegin or
// BlockEnd, or the managed-block scanner would match the comment.

const bashInitScript = `# argmax shell integration for bash.
#
# Usage: eval "$(argmax init bash)"
# or run "argmax setup" to append this integration to ~/.bashrc.
#
# When installed by "argmax setup", everything between the markers
# "# >>> argmax >>>" and "# <<< argmax <<<" is managed: setup upgrades the
# block in place and "argmax uninstall" removes exactly that block, leaving
# all other configuration untouched. Do not edit between the markers.
#
# This script must never print to stdout or stderr.

# --- session marker hygiene -------------------------------------------------
# ARGMAX_SESSION holds the PID of the argmax wrapper. tmux panes (and
# anything else inheriting a dead wrapper's environment) can carry a stale
# marker: if the PID is gone, drop the marker so this shell is treated as
# fresh (RUN-010, PRD section 16).
if [ -n "${ARGMAX_SESSION:-}" ] && ! kill -0 "$ARGMAX_SESSION" 2>/dev/null; then
    unset ARGMAX_SESSION
fi

if [ -n "${ARGMAX_SESSION:-}" ]; then
    # === inside a live argmax session: report events to the wrapper ========
    # The session hands the shell a pipe on an inherited fd (ARGMAX_HOOK_FD,
    # default 3). Hooks write NUL-delimited "<type>\t<payload>" records to it
    # (SH-006). Activate only when the fd is configured and survives a
    # writability probe; probe failure must stay silent.
    __argmax_fd="${ARGMAX_HOOK_FD:-3}"
    if [ -n "${ARGMAX_HOOK_FD:-}" ] && ( : >&"$__argmax_fd" ) 2>/dev/null; then

        __argmax_emit() {
            # $1 = record type, $2 = payload. Never fails noisily.
            command printf '%s\t%s\0' "$1" "$2" >&"$__argmax_fd" 2>/dev/null
        }

        __argmax_last_cwd=""

        # Runs before every prompt. "local __e=$?" MUST stay the first
        # statement so the previous command's exit status survives. Because
        # PROMPT_COMMAND entries run in order, argmax goes first when the
        # variable is empty and appends otherwise (SH-004).
        __argmax_prompt() {
            local __e=$?
            __argmax_emit prompt "$__e"
            if [ "$__argmax_last_cwd" != "$PWD" ]; then
                __argmax_last_cwd="$PWD"
                __argmax_emit cwd "$PWD"
            fi
            return "$__e"
        }

        # Compose with an existing PROMPT_COMMAND — string or bash >= 5.1
        # array — without ever adding our entry twice. __ARGMAX_HOOKED also
        # protects against re-sourcing this file (SH-004).
        if [ -z "${__ARGMAX_HOOKED:-}" ]; then
            __ARGMAX_HOOKED=1
            if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
                __argmax_present=0
                for __argmax_pc in "${PROMPT_COMMAND[@]}"; do
                    if [ "$__argmax_pc" = "__argmax_prompt" ]; then
                        __argmax_present=1
                        break
                    fi
                done
                if [ "$__argmax_present" = 0 ]; then
                    PROMPT_COMMAND+=("__argmax_prompt")
                fi
                unset __argmax_present __argmax_pc
            elif [ -z "${PROMPT_COMMAND:-}" ]; then
                PROMPT_COMMAND="__argmax_prompt"
            else
                case ";${PROMPT_COMMAND%;};" in
                    *";__argmax_prompt;"*) ;;
                    *) PROMPT_COMMAND="${PROMPT_COMMAND%;};__argmax_prompt" ;;
                esac
            fi
        fi
        # Command-start detection is the session's job via foreground
        # process-group detection (RUN-006); deliberately no DEBUG trap here.
    fi
else
    # === standalone shell: autostart argmax ================================
    # Only in an interactive, non-nested, non-rescue shell (SH-002), and only
    # when argmax is on PATH; otherwise leave the shell fully functional.
    if [[ $- == *i* ]] && [ -z "${ARGMAX_RESCUE:-}" ]; then
        if command -v argmax >/dev/null 2>&1; then
            export ARGMAX_SHELL=bash
            exec argmax
        fi
    fi
fi
`

const zshInitScript = `# argmax shell integration for zsh.
#
# Usage: eval "$(argmax init zsh)"
# or run "argmax setup" to append this integration to ~/.zshrc.
#
# When installed by "argmax setup", everything between the markers
# "# >>> argmax >>>" and "# <<< argmax <<<" is managed: setup upgrades the
# block in place and "argmax uninstall" removes exactly that block, leaving
# all other configuration untouched. Do not edit between the markers.
#
# This script must never print to stdout or stderr.

# --- session marker hygiene -------------------------------------------------
# ARGMAX_SESSION holds the PID of the argmax wrapper. tmux panes (and
# anything else inheriting a dead wrapper's environment) can carry a stale
# marker: if the PID is gone, drop the marker so this shell is treated as
# fresh (RUN-010, PRD section 16).
if [ -n "${ARGMAX_SESSION:-}" ] && ! kill -0 "$ARGMAX_SESSION" 2>/dev/null; then
    unset ARGMAX_SESSION
fi

if [ -n "${ARGMAX_SESSION:-}" ]; then
    # === inside a live argmax session: report events to the wrapper ========
    # The session hands the shell a pipe on an inherited fd (ARGMAX_HOOK_FD,
    # default 3). Hooks write NUL-delimited "<type>\t<payload>" records to it
    # (SH-006). Activate only when the fd is configured and survives a
    # writability probe; probe failure must stay silent.
    __argmax_fd="${ARGMAX_HOOK_FD:-3}"
    if [ -n "${ARGMAX_HOOK_FD:-}" ] && ( : >&"$__argmax_fd" ) 2>/dev/null; then

        __argmax_emit() {
            # $1 = record type, $2 = payload. Never fails noisily.
            command printf '%s\t%s\0' "$1" "$2" >&"$__argmax_fd" 2>/dev/null
        }

        __argmax_last_cwd=""

        # precmd: capture $? FIRST, before anything can clobber it (SH-003),
        # and restore it on the way out so prompts showing %? stay correct.
        __argmax_precmd() {
            local __e=$?
            __argmax_emit prompt "$__e"
            if [ "$__argmax_last_cwd" != "$PWD" ]; then
                __argmax_last_cwd="$PWD"
                __argmax_emit cwd "$PWD"
            fi
            return "$__e"
        }

        # preexec: $1 is the command line about to run.
        __argmax_preexec() {
            __argmax_emit preexec "$1"
        }

        if [ -z "${__ARGMAX_HOOKED:-}" ]; then
            __ARGMAX_HOOKED=1
            autoload -Uz add-zsh-hook
            add-zsh-hook precmd __argmax_precmd
            add-zsh-hook preexec __argmax_preexec
        fi

        # --- live buffer reporting (SH-003) -------------------------------
        # Wrap common ZLE widgets so that after every edit the full left
        # buffer is reported as "buffer\t<CURSOR>\t<BUFFER>". Each wrapper
        # calls the dot-prefixed builtin original and then emits. Only
        # widgets that actually exist are wrapped: "zle -l -a" also matches
        # builtins, which plain "zle -l" does not.
        __argmax_widgets=(
            self-insert backward-delete-char delete-char
            backward-kill-word kill-word kill-whole-line kill-line
            backward-kill-line yank quoted-insert self-insert-unmeta
            accept-line beginning-of-line end-of-line
            forward-char backward-char forward-word backward-word
            clear-screen undo redo vi-backward-char vi-forward-char
        )
        for __argmax_w in "${__argmax_widgets[@]}"; do
            if zle -l -a "$__argmax_w" >/dev/null 2>&1; then
                functions[__argmax_wrap_${__argmax_w}]="zle .${__argmax_w}
__argmax_emit buffer \"\$CURSOR\"$'\t'\"\$BUFFER\""
                zle -N "$__argmax_w" "__argmax_wrap_${__argmax_w}"
            fi
        done
        unset __argmax_w __argmax_widgets

        # One initial buffer report when the line editor starts. Do not
        # clobber a user-defined zle-line-init widget.
        if ! zle -l zle-line-init >/dev/null 2>&1; then
            __argmax_line_init() {
                __argmax_emit buffer "$CURSOR"$'\t'"$BUFFER"
            }
            zle -N zle-line-init __argmax_line_init
        fi
    fi
else
    # === standalone shell: autostart argmax ================================
    # Only in an interactive, non-nested, non-rescue shell (SH-002), and only
    # when argmax is on PATH; otherwise leave the shell fully functional.
    if [[ $- == *i* ]] && [ -z "${ARGMAX_RESCUE:-}" ]; then
        if command -v argmax >/dev/null 2>&1; then
            export ARGMAX_SHELL=zsh
            exec argmax
        fi
    fi
fi
`

const fishInitScript = `# argmax shell integration for fish.
#
# Usage: argmax init fish | source
# or run "argmax setup" to append this integration to config.fish.
#
# When installed by "argmax setup", everything between the markers
# "# >>> argmax >>>" and "# <<< argmax <<<" is managed: setup upgrades the
# block in place and "argmax uninstall" removes exactly that block, leaving
# all other configuration untouched. Do not edit between the markers.
#
# This script must never print to stdout or stderr.

# --- session marker hygiene -------------------------------------------------
# ARGMAX_SESSION holds the PID of the argmax wrapper. tmux panes (and
# anything else inheriting a dead wrapper's environment) can carry a stale
# marker: if the PID is gone, drop the marker so this shell is treated as
# fresh (RUN-010, PRD section 16).
if set -q ARGMAX_SESSION
    if not kill -0 $ARGMAX_SESSION 2>/dev/null
        set -e ARGMAX_SESSION
    end
end

if set -q ARGMAX_SESSION
    # === inside a live argmax session: report events to the wrapper ========
    # The session hands the shell a pipe on an inherited fd (ARGMAX_HOOK_FD,
    # default 3). Hooks write NUL-delimited "<type>\t<payload>" records to it
    # (SH-006). Activate only when the fd is configured and survives a
    # writability probe. stderr is redirected BEFORE the fd duplication so a
    # closed fd cannot print an error.
    if set -q ARGMAX_HOOK_FD
        set -g __argmax_fd $ARGMAX_HOOK_FD
        if command printf '' 2>/dev/null >&$__argmax_fd

            function __argmax_emit
                # $argv[1] = record type, $argv[2] = payload. Never noisy.
                command printf '%s\t%s\0' "$argv[1]" "$argv[2]" 2>/dev/null >&$__argmax_fd
            end

            set -g __argmax_last_cwd ""

            function __argmax_prompt --on-event fish_prompt
                # "set -l __e $status" MUST stay the first statement so the
                # previous command's exit status survives (SH-005).
                set -l __e $status
                __argmax_emit prompt $__e
                if test "$__argmax_last_cwd" != "$PWD"
                    set -g __argmax_last_cwd $PWD
                    __argmax_emit cwd "$PWD"
                end
            end

            function __argmax_preexec --on-event fish_preexec
                # $argv is the command line about to run.
                __argmax_emit preexec "$argv"
            end

            function __argmax_postexec --on-event fish_postexec
                set -l __e $status
                __argmax_emit postexec $__e
            end
        end
    end
else if status is-interactive; and not set -q ARGMAX_RESCUE
    # === standalone shell: autostart argmax ================================
    # Only in an interactive, non-nested, non-rescue shell (SH-002), and only
    # when argmax is on PATH; otherwise leave the shell fully functional.
    if command -q argmax
        set -gx ARGMAX_SHELL fish
        exec argmax
    end
end
`
