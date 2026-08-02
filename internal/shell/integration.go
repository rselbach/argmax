package shell

import "fmt"

// Markers delimit the managed autostart block in shell configuration files.
const (
	BeginMarker = "# >>> argmax initialize >>>"
	EndMarker   = "# <<< argmax initialize <<<"
)

// autostartPOSIX is used for bash and zsh rc files.
func autostartPOSIX(k Kind) string {
	return fmt.Sprintf(`%s
# Managed by 'argmax setup'. Remove with 'argmax uninstall'.
if command -v argmax >/dev/null 2>&1; then
  eval "$(argmax init %s)"
fi
%s`, BeginMarker, k, EndMarker)
}

// autostartFish is used for config.fish.
func autostartFish() string {
	return fmt.Sprintf(`%s
# Managed by 'argmax setup'. Remove with 'argmax uninstall'.
if command -q argmax
    argmax init fish | source
end
%s`, BeginMarker, EndMarker)
}

// Block returns the shell-appropriate autostart block.
func Block(k Kind) string {
	if k == Fish {
		return autostartFish()
	}
	return autostartPOSIX(k)
}

// InitScript returns the sourceable integration printed by `argmax init`.
// The script autostarts argmax only in an interactive, non-nested,
// non-rescue shell; inside a wrapped session it defines hooks that report
// shell events over the session-private inherited file descriptor as
// NUL-delimited records.
func InitScript(k Kind) string {
	switch k {
	case Bash:
		return bashInit
	case Zsh:
		return zshInit
	case Fish:
		return fishInit
	}
	return ""
}

const zshInit = `# argmax shell integration for zsh
if [[ -o interactive ]]; then
  # Clear session markers inherited across a tmux or SSH boundary.
  if [[ -n "$ARGMAX_ACTIVE" && "$(tty 2>/dev/null)" != "$ARGMAX_TTY" ]]; then
    unset ARGMAX_ACTIVE ARGMAX_EVENTS_FD ARGMAX_TTY
  fi
  if [[ -z "$ARGMAX_ACTIVE" && -z "$ARGMAX_RESCUE" ]] && command -v argmax >/dev/null 2>&1; then
    exec argmax
  elif [[ -n "$ARGMAX_ACTIVE" && -n "$ARGMAX_EVENTS_FD" ]]; then
    __argmax_emit() { printf '%s\0' "$1" >&"$ARGMAX_EVENTS_FD" 2>/dev/null }
    __argmax_precmd() {
      local st=$?
      __argmax_emit "post:$st"
      __argmax_emit "cwd:$PWD"
      __argmax_emit "ready:"
    }
    __argmax_preexec() { __argmax_emit "pre:$1" }
    __argmax_buf() { __argmax_emit "buf:$LBUFFER" }
    autoload -Uz add-zsh-hook add-zle-hook-widget
    add-zsh-hook precmd __argmax_precmd
    add-zsh-hook preexec __argmax_preexec
    add-zle-hook-widget line-pre-redraw __argmax_buf 2>/dev/null
  fi
fi
`

const bashInit = `# argmax shell integration for bash
case $- in *i*)
  # Clear session markers inherited across a tmux or SSH boundary.
  if [ -n "${ARGMAX_ACTIVE:-}" ] && [ "$(tty 2>/dev/null)" != "${ARGMAX_TTY:-}" ]; then
    unset ARGMAX_ACTIVE ARGMAX_EVENTS_FD ARGMAX_TTY
  fi
  if [ -z "${ARGMAX_ACTIVE:-}" ] && [ -z "${ARGMAX_RESCUE:-}" ] && command -v argmax >/dev/null 2>&1; then
    exec argmax
  elif [ -n "${ARGMAX_ACTIVE:-}" ] && [ -n "${ARGMAX_EVENTS_FD:-}" ]; then
    __argmax_emit() { printf '%s\0' "$1" >&"${ARGMAX_EVENTS_FD}" 2>/dev/null; }
    __argmax_prompt() {
      local st=$?
      __argmax_emit "post:${st}"
      __argmax_emit "cwd:${PWD}"
      __argmax_emit "ready:"
      __argmax_ran=
    }
    __argmax_debug() {
      [ -n "${COMP_LINE:-}" ] && return
      case "${BASH_COMMAND}" in __argmax_*) return ;; esac
      if [ -z "${__argmax_ran:-}" ]; then
        __argmax_ran=1
        __argmax_emit "pre:${BASH_COMMAND}"
      fi
    }
    case ";${PROMPT_COMMAND:-};" in
      *";__argmax_prompt;"*) ;;
      *) PROMPT_COMMAND="__argmax_prompt${PROMPT_COMMAND:+;${PROMPT_COMMAND}}" ;;
    esac
    trap '__argmax_debug' DEBUG
  fi
  ;;
esac
`

const fishInit = `# argmax shell integration for fish
if status is-interactive
    # Clear session markers inherited across a tmux or SSH boundary.
    if set -q ARGMAX_ACTIVE; and test (tty 2>/dev/null) != "$ARGMAX_TTY"
        set -e ARGMAX_ACTIVE
        set -e ARGMAX_EVENTS_FD
        set -e ARGMAX_TTY
    end
    if not set -q ARGMAX_ACTIVE; and not set -q ARGMAX_RESCUE; and command -q argmax
        exec argmax
    else if set -q ARGMAX_ACTIVE; and set -q ARGMAX_EVENTS_FD
        # The event descriptor is always fd 3; fish needs a literal fd in
        # redirections.
        function __argmax_emit
            printf '%s\0' $argv[1] >&3 2>/dev/null
        end
        function __argmax_preexec --on-event fish_preexec
            __argmax_emit "pre:$argv"
        end
        function __argmax_postexec --on-event fish_postexec
            __argmax_emit "post:$status"
        end
        function __argmax_prompt --on-event fish_prompt
            __argmax_emit "cwd:$PWD"
            __argmax_emit "ready:"
        end
    end
end
`
