//! Pure shell-integration generation and shell-config editing.
//!
//! This module performs no filesystem access. Callers remain responsible for
//! backups, atomic writes, and preserving file ownership and permissions.

use std::error::Error;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub use crate::config::Shell;

/// Stable opening marker shared by setup, migration, and uninstall.
pub const BEGIN_MARKER: &str = "# >>> argmax shell integration >>>";

/// Stable closing marker shared by setup, migration, and uninstall.
pub const END_MARKER: &str = "# <<< argmax shell integration <<<";

/// Environment marker set only for a shell already owned by an argmax session.
pub const SESSION_MARKER_ENV: &str = "ARGMAX_PRIVATE_SESSION";

/// Process binding for the shell that owns the private session event channel.
pub const SESSION_OWNER_PID_ENV: &str = "ARGMAX_SESSION_OWNER_PID";

/// Reserved byte sequence used to request a shell-native editing snapshot.
///
/// The wrapper may inject it only at a safe editing boundary, never during a
/// paste, foreground command, or after Enter.
pub const SYNC_PROBE_SEQUENCE: &[u8] = b"\x1b[argmax-sync~";

/// Maximum characters an adapter will synchronously copy into one event.
pub const MAX_SYNC_EVENT_CHARACTERS: usize = 16 * 1024;

/// Shell-native mechanism available for authoritative live-buffer snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferSyncAdapter {
    /// A native redraw hook whose ordering must be proven by the controller.
    NativeRedraw,
    /// A collision-free private binding responds to [`SYNC_PROBE_SEQUENCE`].
    ReservedProbe,
}

/// Fidelity of command text available at the shell preexec boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandTextAdapter {
    /// The preexec callback supplies exact submitted command text.
    ExactPreexec,
    /// This shell adapter cannot verify exact submitted command text.
    Unavailable,
}

/// Static integration capabilities for one supported shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntegrationCapabilities {
    /// Mechanism used for live editing snapshots.
    pub buffer_sync: BufferSyncAdapter,
    /// Mechanism used to attribute a submitted command.
    pub command_text: CommandTextAdapter,
}

/// Returns the truthful static capabilities of a generated shell adapter.
///
/// Probe-based adapters announce `capability:unavailable` at runtime when the
/// reserved sequence already has a user binding or installation fails.
/// Bash command text is deliberately unavailable: PS0 cannot prove that a
/// prior snapshot is the command submitted by a custom Return binding or a
/// multiline editor action, so Bash completions must not feed exact learning.
#[must_use]
pub const fn integration_capabilities(shell: Shell) -> IntegrationCapabilities {
    match shell {
        Shell::Bash => IntegrationCapabilities {
            buffer_sync: BufferSyncAdapter::ReservedProbe,
            command_text: CommandTextAdapter::Unavailable,
        },
        Shell::Zsh | Shell::Fish => IntegrationCapabilities {
            buffer_sync: BufferSyncAdapter::ReservedProbe,
            command_text: CommandTextAdapter::ExactPreexec,
        },
    }
}

const BASH_INIT: &str = r#"# argmax shell integration
if [[ $- == *i* && -t 0 && -t 1 ]]; then
  if [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ -z ${ARGMAX_SESSION_OWNER_PID-} ]]; then
      export ARGMAX_SESSION_OWNER_PID=$BASHPID
    elif [[ $ARGMAX_SESSION_OWNER_PID != "$BASHPID" ]]; then
      unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL \
        ARGMAX_SESSION_OWNER_PID
    fi
  fi

  if [[ -z ${ARGMAX_PRIVATE_SESSION-} &&
        -z ${BASH_EXECUTION_STRING-} && $# -eq 0 ]]; then
    if command -v argmax >/dev/null 2>&1; then
      export ARGMAX_ACTIVE_SHELL=bash
      exec argmax --shell bash
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -z ${__ARGMAX_BASH_HOOKS-} ]]; then
    if (( BASH_VERSINFO[0] < 4 ||
          (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
      if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
          (( 10#$ARGMAX_EVENT_FD >= 3 )); then
        printf '%s\0' capability:unavailable \
          2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
      fi
    elif declare -F __argmax_emit >/dev/null ||
        declare -F __argmax_preexec >/dev/null ||
        declare -F __argmax_precmd >/dev/null ||
        declare -F __argmax_sync >/dev/null ||
        declare -F __argmax_probe_is_unbound >/dev/null ||
        declare -F __argmax_install >/dev/null ||
        [[ -n ${__ARGMAX_BASH_HOOKS+x} ||
           -n ${__ARGMAX_BASH_CAPABILITY+x} ||
           -n ${__ARGMAX_BASH_COMMAND_ACTIVE+x} ||
           -n ${__ARGMAX_BASH_PROBE+x} ||
           -n ${__ARGMAX_BASH_PROBE_NONCE+x} ||
           ${PS0-} == *'__argmax_preexec'* ||
           ${PROMPT_COMMAND[*]-} == *'__argmax_precmd'* ]]; then
      if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
          (( 10#$ARGMAX_EVENT_FD >= 3 )); then
        printf '%s\0' capability:unavailable \
          2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
      fi
    else
      __argmax_install() {
        local argmax_install_ok=1
        local argmax_ps0_set=0
        local argmax_ps0_value=
        local argmax_ps0_declaration
        local argmax_prompt_kind=unset
        local argmax_prompt_scalar=
        local argmax_prompt_declaration
        local -a argmax_prompt_indices=()
        local -a argmax_prompt_values=()
        local argmax_emacs_attempted=0
        local argmax_vi_insert_attempted=0
        local argmax_vi_command_attempted=0
        local argmax_ps0_attempted=0
        local argmax_prompt_attempted=0
        local argmax_index
        local argmax_restore_index

        if argmax_ps0_declaration=$(builtin declare -p PS0 2>/dev/null); then
          if [[ $argmax_ps0_declaration != 'declare -- PS0' &&
                $argmax_ps0_declaration != 'declare -- PS0='* ]]; then
            return 1
          fi
          argmax_ps0_set=1
          argmax_ps0_value=$PS0
        fi
        if argmax_prompt_declaration=$(
            builtin declare -p PROMPT_COMMAND 2>/dev/null
          ); then
          if [[ $argmax_prompt_declaration == 'declare -- PROMPT_COMMAND' ||
                $argmax_prompt_declaration == \
                  'declare -- PROMPT_COMMAND='* ]]; then
            argmax_prompt_kind=scalar
            argmax_prompt_scalar=$PROMPT_COMMAND
          elif [[ $argmax_prompt_declaration == \
                    'declare -a PROMPT_COMMAND' ||
                  $argmax_prompt_declaration == \
                    'declare -a PROMPT_COMMAND='* ]]; then
            argmax_prompt_kind=array
            argmax_prompt_indices=("${!PROMPT_COMMAND[@]}")
            argmax_prompt_values=("${PROMPT_COMMAND[@]}")
          else
            return 1
          fi
        fi

        if ! __ARGMAX_BASH_HOOKS=argmax-owned-bash-v1 ||
            ! __ARGMAX_BASH_PROBE=$'\e[argmax-sync~' ||
            ! __ARGMAX_BASH_PROBE_NONCE=0; then
          argmax_install_ok=0
        fi

        __argmax_emit() {
          : argmax-owned-bash-v1
          local argmax_event=$1
          [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] || return 0
          (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
          if (( ${#argmax_event} > 16384 )); then
            argmax_event=protocol-frame-oversized
          fi
          printf '%s\0' "$argmax_event" \
            2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
        }

        __argmax_preexec() {
          : argmax-owned-bash-v1
          # PS0 cannot verify custom Return bindings or multiline submissions.
          __argmax_emit command-start-unknown
        }

        __argmax_precmd() {
          local argmax_status=$?
          : argmax-owned-bash-v1
          if [[ -n ${__ARGMAX_BASH_COMMAND_ACTIVE+x} ]]; then
            __argmax_emit "command-stop:$argmax_status"
            builtin unset __ARGMAX_BASH_COMMAND_ACTIVE
          fi
          __argmax_emit prompt-ready
          return "$argmax_status"
        }

        __argmax_sync() {
          : argmax-owned-bash-v1
          local argmax_buffer=$READLINE_LINE
          local argmax_locale=${LC_ALL-}
          local argmax_unit=c
          if [[ -z $argmax_locale ]]; then
            argmax_locale=${LC_CTYPE-}
          fi
          if [[ -z $argmax_locale ]]; then
            argmax_locale=${LANG-}
          fi
          if [[ $argmax_locale == C || $argmax_locale == POSIX ]]; then
            argmax_unit=b
          fi
          if (( ${#argmax_buffer} > 16384 )); then
            __argmax_emit protocol-frame-oversized
            return 0
          fi
          if (( __ARGMAX_BASH_PROBE_NONCE == 9223372036854775807 )); then
            __ARGMAX_BASH_CAPABILITY=unavailable
            __argmax_emit capability:unavailable
            return 0
          fi
          ((__ARGMAX_BASH_PROBE_NONCE += 1))
          __argmax_emit \
            "probe-buffer:$argmax_unit:$__ARGMAX_BASH_PROBE_NONCE:$READLINE_POINT:$argmax_buffer"
        }

        __argmax_probe_is_unbound() {
          : argmax-owned-bash-v1
          local argmax_keymap=$1
          local argmax_binding
          local argmax_bindings
          local argmax_more
          argmax_bindings=$(builtin bind -m "$argmax_keymap" -p 2>/dev/null) ||
            return 1
          argmax_more=$(builtin bind -m "$argmax_keymap" -s 2>/dev/null) ||
            return 1
          argmax_bindings+=$'\n'$argmax_more
          argmax_more=$(builtin bind -m "$argmax_keymap" -X 2>/dev/null) ||
            return 1
          argmax_bindings+=$'\n'$argmax_more
          while IFS= read -r argmax_binding; do
            case $argmax_binding in
              *'"\e[argmax-sync~"'*) return 1 ;;
            esac
          done <<< "$argmax_bindings"
          return 0
        }

        if (( argmax_install_ok )) &&
            (! builtin declare -F __argmax_emit >/dev/null ||
             ! builtin declare -F __argmax_preexec >/dev/null ||
             ! builtin declare -F __argmax_precmd >/dev/null ||
             ! builtin declare -F __argmax_sync >/dev/null ||
             ! builtin declare -F __argmax_probe_is_unbound >/dev/null); then
          argmax_install_ok=0
        fi
        if (( argmax_install_ok )) &&
            (! __argmax_probe_is_unbound emacs-standard ||
             ! __argmax_probe_is_unbound vi-insert ||
             ! __argmax_probe_is_unbound vi-command); then
          argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_emacs_attempted=1
          builtin bind -m emacs-standard -x \
            '"\e[argmax-sync~":__argmax_sync' 2>/dev/null ||
            argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_vi_insert_attempted=1
          builtin bind -m vi-insert -x \
            '"\e[argmax-sync~":__argmax_sync' 2>/dev/null ||
            argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_vi_command_attempted=1
          builtin bind -m vi-command -x \
            '"\e[argmax-sync~":__argmax_sync' 2>/dev/null ||
            argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_ps0_attempted=1
          # Set the marker in the parent shell before the emitting substitution.
          # shellcheck disable=SC2016 # deliberately deferred to Bash
          PS0=${PS0-}'${__ARGMAX_BASH_COMMAND_ACTIVE:=}$(__argmax_preexec)' ||
            argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_prompt_attempted=1
          if [[ $argmax_prompt_kind == array ]]; then
            PROMPT_COMMAND=(__argmax_precmd "${PROMPT_COMMAND[@]}") ||
              argmax_install_ok=0
          else
            # shellcheck disable=SC2128,SC2178 # scalar form
            PROMPT_COMMAND="__argmax_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ||
              argmax_install_ok=0
          fi
        fi
        if (( argmax_install_ok )); then
          __ARGMAX_BASH_CAPABILITY=probe || argmax_install_ok=0
        fi

        if (( argmax_install_ok )); then
          __argmax_emit \
            "capability:sync-probe:$__ARGMAX_BASH_PROBE_NONCE"
          return 0
        fi

        if (( argmax_prompt_attempted )); then
          case $argmax_prompt_kind in
            array)
              PROMPT_COMMAND=()
              for argmax_index in "${!argmax_prompt_indices[@]}"; do
                argmax_restore_index=${argmax_prompt_indices[$argmax_index]}
                PROMPT_COMMAND[argmax_restore_index]=${argmax_prompt_values[$argmax_index]}
              done
              ;;
            scalar)
              # shellcheck disable=SC2178 # restoring original scalar form
              PROMPT_COMMAND=$argmax_prompt_scalar
              ;;
            unset) builtin unset PROMPT_COMMAND 2>/dev/null || : ;;
          esac
        fi
        if (( argmax_ps0_attempted )); then
          if (( argmax_ps0_set )); then
            PS0=$argmax_ps0_value
          else
            builtin unset PS0 2>/dev/null || :
          fi
        fi
        (( argmax_vi_command_attempted )) &&
          builtin bind -m vi-command -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        (( argmax_vi_insert_attempted )) &&
          builtin bind -m vi-insert -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        (( argmax_emacs_attempted )) &&
          builtin bind -m emacs-standard -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        builtin unset -f __argmax_emit __argmax_preexec __argmax_precmd \
          __argmax_sync __argmax_probe_is_unbound
        builtin unset __ARGMAX_BASH_HOOKS __ARGMAX_BASH_CAPABILITY \
          __ARGMAX_BASH_COMMAND_ACTIVE __ARGMAX_BASH_PROBE \
          __ARGMAX_BASH_PROBE_NONCE
        return 1
      }

      if ! __argmax_install; then
        if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
            (( 10#$ARGMAX_EVENT_FD >= 3 )); then
          printf '%s\0' capability:unavailable \
            2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
        fi
      fi
      builtin unset -f __argmax_install
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          ${__ARGMAX_BASH_HOOKS-} == argmax-owned-bash-v1 &&
          $(declare -f __argmax_emit 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_preexec 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_precmd 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_sync 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_probe_is_unbound 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          ${__ARGMAX_BASH_COMMAND_ACTIVE-} == '' &&
          ${PS0-} == *'__argmax_preexec'* &&
          ${PROMPT_COMMAND[*]-} == *'__argmax_precmd'* &&
          $(bind -m emacs-standard -X 2>/dev/null) == \
            *argmax-sync~*__argmax_sync* &&
          $(bind -m vi-insert -X 2>/dev/null) == \
            *argmax-sync~*__argmax_sync* &&
          $(bind -m vi-command -X 2>/dev/null) == \
            *argmax-sync~*__argmax_sync* ]]; then
    if [[ ${__ARGMAX_BASH_CAPABILITY-} == probe ]]; then
      __argmax_emit "capability:sync-probe:${__ARGMAX_BASH_PROBE_NONCE-0}"
    else
      __argmax_emit capability:unavailable
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
        (( 10#$ARGMAX_EVENT_FD >= 3 )); then
      printf '%s\0' capability:unavailable \
        2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
    fi
  fi
fi
"#;

const ZSH_INIT: &str = r#"# argmax shell integration
if [[ -o interactive && -t 0 && -t 1 ]]; then
  if [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ -z ${ARGMAX_SESSION_OWNER_PID-} ]]; then
      export ARGMAX_SESSION_OWNER_PID=$$
    elif [[ $ARGMAX_SESSION_OWNER_PID != "$$" ]]; then
      unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL \
        ARGMAX_SESSION_OWNER_PID
    fi
  fi

  if [[ -z ${ARGMAX_PRIVATE_SESSION-} &&
        -z ${ZSH_EXECUTION_STRING-} && $# -eq 0 ]]; then
    if (( $+commands[argmax] )); then
      export ARGMAX_ACTIVE_SHELL=zsh
      exec argmax --shell zsh
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -z ${__ARGMAX_ZSH_HOOKS-} ]]; then
    if (( $+functions[__argmax_emit] ||
          $+functions[__argmax_preexec] ||
          $+functions[__argmax_precmd] ||
          $+functions[__argmax_sync] ||
          $+parameters[__ARGMAX_ZSH_HOOKS] ||
          $+parameters[__ARGMAX_ZSH_CAPABILITY] ||
          $+parameters[__ARGMAX_ZSH_COMMAND_ACTIVE] ||
          $+parameters[__ARGMAX_ZSH_PROBE] ||
          $+parameters[__ARGMAX_ZSH_PROBE_NONCE] ||
          $+parameters[__argmax_zsh_preexec_added] ||
          $+parameters[__argmax_zsh_precmd_added] ||
          $+parameters[__argmax_zsh_widget_added] ||
          $+parameters[__argmax_zsh_install_ok] ||
          $+parameters[__argmax_zsh_bound_maps] ||
          $+parameters[__argmax_zsh_binding] ||
          $+parameters[__argmax_zsh_map] ||
          $+widgets[__argmax_sync] )) ||
        [[ ${preexec_functions[(r)__argmax_preexec]-} == __argmax_preexec ||
           ${precmd_functions[(r)__argmax_precmd]-} == __argmax_precmd ]]; then
      if [[ ${ARGMAX_EVENT_FD-} == <-> ]] &&
          (( 10#$ARGMAX_EVENT_FD >= 3 )); then
        print -rn -- capability:unavailable$'\0' \
          2>/dev/null 1>&$ARGMAX_EVENT_FD || :
      fi
    else
      __ARGMAX_ZSH_HOOKS=argmax-owned-zsh-v1
      __ARGMAX_ZSH_COMMAND_ACTIVE=0
      __ARGMAX_ZSH_PROBE=$'\e[argmax-sync~'
      __ARGMAX_ZSH_PROBE_NONCE=0

      __argmax_emit() {
        : argmax-owned-zsh-v1
        local argmax_event=$1
        [[ ${ARGMAX_EVENT_FD-} == <-> ]] || return 0
        (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
        if (( ${#argmax_event} > 16384 )); then
          argmax_event=protocol-frame-oversized
        fi
        print -rn -- "$argmax_event"$'\0' \
          2>/dev/null 1>&$ARGMAX_EVENT_FD || :
      }

      __argmax_preexec() {
        : argmax-owned-zsh-v1
        __ARGMAX_ZSH_COMMAND_ACTIVE=1
        if [[ -n $1 ]]; then
          __argmax_emit "command-start:$1"
        else
          __argmax_emit command-start-unknown
        fi
      }

      __argmax_precmd() {
        local argmax_status=$?
        : argmax-owned-zsh-v1
        if (( ${__ARGMAX_ZSH_COMMAND_ACTIVE:-0} )); then
          __argmax_emit "command-stop:$argmax_status"
          __ARGMAX_ZSH_COMMAND_ACTIVE=0
        fi
        __argmax_emit prompt-ready
        return $argmax_status
      }

      __argmax_sync() {
        : argmax-owned-zsh-v1
        local argmax_unit=b
        if [[ -o multibyte ]]; then
          argmax_unit=c
        fi
        if (( ${#BUFFER} > 16384 )); then
          __argmax_emit protocol-frame-oversized
          return 0
        fi
        if (( __ARGMAX_ZSH_PROBE_NONCE == 9223372036854775807 )); then
          __ARGMAX_ZSH_CAPABILITY=unavailable
          __argmax_emit capability:unavailable
          return 0
        fi
        ((__ARGMAX_ZSH_PROBE_NONCE += 1))
        __argmax_emit \
          "probe-buffer:$argmax_unit:$__ARGMAX_ZSH_PROBE_NONCE:$CURSOR:$BUFFER"
      }

      autoload -Uz add-zsh-hook
      typeset -i __argmax_zsh_preexec_added=0
      typeset -i __argmax_zsh_precmd_added=0
      typeset -i __argmax_zsh_widget_added=0
      typeset -i __argmax_zsh_install_ok=1
      typeset -a __argmax_zsh_bound_maps=()
      typeset __argmax_zsh_binding

      for __argmax_zsh_map in emacs viins vicmd; do
        __argmax_zsh_binding=$(bindkey -M "$__argmax_zsh_map" \
          "$__ARGMAX_ZSH_PROBE" 2>/dev/null) || __argmax_zsh_install_ok=0
        if (( __argmax_zsh_install_ok )) &&
            [[ $__argmax_zsh_binding != *undefined-key ]]; then
          __argmax_zsh_install_ok=0
        fi
      done
      if (( __argmax_zsh_install_ok )); then
        add-zsh-hook preexec __argmax_preexec &&
          __argmax_zsh_preexec_added=1 || __argmax_zsh_install_ok=0
      fi
      if (( __argmax_zsh_install_ok )); then
        add-zsh-hook precmd __argmax_precmd &&
          __argmax_zsh_precmd_added=1 || __argmax_zsh_install_ok=0
      fi
      if (( __argmax_zsh_install_ok )); then
        zle -N __argmax_sync &&
          __argmax_zsh_widget_added=1 || __argmax_zsh_install_ok=0
      fi
      if (( __argmax_zsh_install_ok )); then
        for __argmax_zsh_map in emacs viins vicmd; do
          if bindkey -M "$__argmax_zsh_map" \
              "$__ARGMAX_ZSH_PROBE" __argmax_sync; then
            __argmax_zsh_bound_maps+=("$__argmax_zsh_map")
          else
            __argmax_zsh_install_ok=0
            break
          fi
        done
      fi

      if (( __argmax_zsh_install_ok )); then
        __ARGMAX_ZSH_CAPABILITY=probe
        __argmax_emit \
          "capability:sync-probe:$__ARGMAX_ZSH_PROBE_NONCE"
      else
        for __argmax_zsh_map in $__argmax_zsh_bound_maps; do
          bindkey -M "$__argmax_zsh_map" \
            -r "$__ARGMAX_ZSH_PROBE" 2>/dev/null || :
        done
        (( __argmax_zsh_widget_added )) &&
          zle -D __argmax_sync 2>/dev/null || :
        (( __argmax_zsh_precmd_added )) &&
          add-zsh-hook -d precmd __argmax_precmd 2>/dev/null || :
        (( __argmax_zsh_preexec_added )) &&
          add-zsh-hook -d preexec __argmax_preexec 2>/dev/null || :
        if [[ ${ARGMAX_EVENT_FD-} == <-> ]] &&
            (( 10#$ARGMAX_EVENT_FD >= 3 )); then
          print -rn -- capability:unavailable$'\0' \
            2>/dev/null 1>&$ARGMAX_EVENT_FD || :
        fi
        unfunction __argmax_emit __argmax_preexec __argmax_precmd \
          __argmax_sync 2>/dev/null || :
        unset __ARGMAX_ZSH_HOOKS __ARGMAX_ZSH_COMMAND_ACTIVE \
          __ARGMAX_ZSH_CAPABILITY __ARGMAX_ZSH_PROBE \
          __ARGMAX_ZSH_PROBE_NONCE
      fi
      unset __argmax_zsh_preexec_added __argmax_zsh_precmd_added \
        __argmax_zsh_widget_added __argmax_zsh_install_ok \
        __argmax_zsh_bound_maps __argmax_zsh_binding __argmax_zsh_map
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -n ${__ARGMAX_ZSH_HOOKS-} ]] &&
      [[ ${__ARGMAX_ZSH_HOOKS-} == argmax-owned-zsh-v1 &&
         $(functions __argmax_emit 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_preexec 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_precmd 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_sync 2>/dev/null) == *argmax-owned-zsh-v1* &&
         ${+widgets[__argmax_sync]} == 1 &&
         ${preexec_functions[(r)__argmax_preexec]-} == __argmax_preexec &&
         ${precmd_functions[(r)__argmax_precmd]-} == __argmax_precmd &&
         $(bindkey -M emacs "${__ARGMAX_ZSH_PROBE-}" 2>/dev/null) == \
           *__argmax_sync* &&
         $(bindkey -M viins "${__ARGMAX_ZSH_PROBE-}" 2>/dev/null) == \
           *__argmax_sync* &&
         $(bindkey -M vicmd "${__ARGMAX_ZSH_PROBE-}" 2>/dev/null) == \
           *__argmax_sync* ]]; then
    if [[ ${__ARGMAX_ZSH_CAPABILITY-} == probe ]]; then
      __argmax_emit \
        "capability:sync-probe:${__ARGMAX_ZSH_PROBE_NONCE-0}"
    else
      __argmax_emit capability:unavailable
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ ${ARGMAX_EVENT_FD-} == <-> ]] &&
        (( 10#$ARGMAX_EVENT_FD >= 3 )); then
      print -rn -- capability:unavailable$'\0' \
        2>/dev/null 1>&$ARGMAX_EVENT_FD || :
    fi
  fi
fi
"#;

const FISH_INIT: &str = r#"# argmax shell integration
if status is-interactive; and test -t 0; and test -t 1
  if set -q ARGMAX_PRIVATE_SESSION
    if not set -q ARGMAX_SESSION_OWNER_PID
      set -gx ARGMAX_SESSION_OWNER_PID $fish_pid
    else if test "$ARGMAX_SESSION_OWNER_PID" != "$fish_pid"
      set -e ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL \
        ARGMAX_SESSION_OWNER_PID
    end
  end

  if not set -q ARGMAX_PRIVATE_SESSION
    if command -q argmax
      set -gx ARGMAX_ACTIVE_SHELL fish
      exec argmax --shell fish
    end
  else
    set -l argmax_function_collision 0
    set -l argmax_functions __argmax_emit __argmax_sync __argmax_preexec \
      __argmax_postexec __argmax_posterror __argmax_prompt
    if set -q __ARGMAX_FISH_INSTALLED
      if test "$__ARGMAX_FISH_INSTALLED" != argmax-owned-fish-v1; or \
          not set -q __ARGMAX_FISH_CAPABILITY; or \
          test "$__ARGMAX_FISH_CAPABILITY" != probe; or \
          not set -q __ARGMAX_FISH_COMMAND_ACTIVE; or \
          not set -q __ARGMAX_FISH_PROBE_NONCE; or not \
          string match -qr '^[0-9]+$' -- $__ARGMAX_FISH_PROBE_NONCE
        set argmax_function_collision 1
      end
      for argmax_function in $argmax_functions
        if not functions -q $argmax_function
          set argmax_function_collision 1
        else
          functions $argmax_function | \
            string match -q '*argmax-owned-fish-v1*'; or \
            set argmax_function_collision 1
        end
      end
    else
      if set -q __ARGMAX_FISH_COMMAND_ACTIVE; or \
          set -q __ARGMAX_FISH_PROBE_NONCE; or \
          set -q __ARGMAX_FISH_CAPABILITY
        set argmax_function_collision 1
      end
      for argmax_function in $argmax_functions
        if functions -q $argmax_function
          set argmax_function_collision 1
        end
      end
    end

    if test $argmax_function_collision = 1
      if set -q ARGMAX_EVENT_FD; and \
          string match -qr '^[0-9]+$' -- $ARGMAX_EVENT_FD; and \
          test $ARGMAX_EVENT_FD -ge 3
        printf '%s\0' capability:unavailable \
          2>/dev/null 1>&$ARGMAX_EVENT_FD; or true
      end
    else
      functions -e $argmax_functions 2>/dev/null

    function __argmax_emit
      set -l __argmax_fish_owner argmax-owned-fish-v1
      set -q ARGMAX_EVENT_FD; or return 0
      string match -qr '^[0-9]+$' -- $ARGMAX_EVENT_FD; or return 0
      test $ARGMAX_EVENT_FD -ge 3; or return 0
      set -l argmax_event "$argv[1]"
      if test (string length -- "$argmax_event") -gt 16384
        set argmax_event protocol-frame-oversized
      end
      printf '%s\0' "$argmax_event" 2>/dev/null 1>&$ARGMAX_EVENT_FD; or true
    end

    function __argmax_preexec --on-event fish_preexec
      set -l __argmax_fish_owner argmax-owned-fish-v1
      set -g __ARGMAX_FISH_COMMAND_ACTIVE 1
      if test (count $argv) -gt 0; and test -n "$argv[1]"
        __argmax_emit "command-start:$argv[1]"
      else
        __argmax_emit command-start-unknown
      end
    end

    function __argmax_postexec --on-event fish_postexec
      set -l argmax_status $status
      set -l __argmax_fish_owner argmax-owned-fish-v1
      __argmax_emit "command-stop:$argmax_status"
      set -g __ARGMAX_FISH_COMMAND_ACTIVE 0
      return $argmax_status
    end

    function __argmax_posterror --on-event fish_posterror
      set -l argmax_status $status
      set -l __argmax_fish_owner argmax-owned-fish-v1
      if test "$__ARGMAX_FISH_COMMAND_ACTIVE" != 1
        __argmax_emit command-start-unknown
      end
      __argmax_emit "command-stop:$argmax_status"
      set -g __ARGMAX_FISH_COMMAND_ACTIVE 0
      return $argmax_status
    end

    function __argmax_prompt --on-event fish_prompt
      set -l __argmax_fish_owner argmax-owned-fish-v1
      set -g __ARGMAX_FISH_COMMAND_ACTIVE 0
      __argmax_emit prompt-ready
    end

    function __argmax_sync
      set -l __argmax_fish_owner argmax-owned-fish-v1
      # commandline prints one newline; the decoder removes only that terminator.
      set -l argmax_buffer (commandline -b | string collect -N)
      set -l argmax_cursor (commandline -C)
      if test (string length -- "$argmax_buffer") -gt 16384
        __argmax_emit protocol-frame-oversized
        return 0
      end
      if test $__ARGMAX_FISH_PROBE_NONCE -ge 2147483647
        set -g __ARGMAX_FISH_CAPABILITY unavailable
        __argmax_emit capability:unavailable
        return 0
      end
      set -g __ARGMAX_FISH_PROBE_NONCE \
        (math $__ARGMAX_FISH_PROBE_NONCE + 1)
      __argmax_emit \
        "probe-buffer:f:$__ARGMAX_FISH_PROBE_NONCE:$argmax_cursor:$argmax_buffer"
    end

    set -g __ARGMAX_FISH_COMMAND_ACTIVE 0
    if not set -q __ARGMAX_FISH_PROBE_NONCE
      set -g __ARGMAX_FISH_PROBE_NONCE 0
    end
    set -l argmax_probe \e\[argmax-sync~
    set -l argmax_probe_available 1
    set -l argmax_probe_modes (bind --list-modes)
    set -l argmax_registered_modes
    if test (count $argmax_probe_modes) -eq 0
      set argmax_probe_available 0
    end
    for argmax_mode in $argmax_probe_modes
      set -l argmax_binding \
        (bind --mode $argmax_mode $argmax_probe 2>/dev/null |
          string collect -N)
      if test -n "$argmax_binding"
        if string match -rq \
            '[[:space:]]__argmax_sync[[:space:]]*$' -- \
            "$argmax_binding"
          bind --erase --mode $argmax_mode $argmax_probe 2>/dev/null; or \
            set argmax_probe_available 0
        else
          set argmax_probe_available 0
        end
      end
    end
    if test $argmax_probe_available = 1
      for argmax_mode in $argmax_probe_modes
        if bind --mode $argmax_mode $argmax_probe __argmax_sync
          set -a argmax_registered_modes $argmax_mode
        else
          set argmax_probe_available 0
          break
        end
      end
    end
    if test $argmax_probe_available = 1
      set -g __ARGMAX_FISH_INSTALLED argmax-owned-fish-v1
      set -g __ARGMAX_FISH_CAPABILITY probe
      __argmax_emit \
        "capability:sync-probe:$__ARGMAX_FISH_PROBE_NONCE"
    else
      for argmax_mode in $argmax_registered_modes
        bind --erase --mode $argmax_mode $argmax_probe 2>/dev/null
      end
      __argmax_emit capability:unavailable
      functions -e $argmax_functions 2>/dev/null
      set -e __ARGMAX_FISH_INSTALLED __ARGMAX_FISH_CAPABILITY \
        __ARGMAX_FISH_COMMAND_ACTIVE __ARGMAX_FISH_PROBE_NONCE
    end
    end
  end
end
"#;

/// Returns sourceable integration code and no human-oriented explanation.
#[must_use]
pub const fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => BASH_INIT,
        Shell::Zsh => ZSH_INIT,
        Shell::Fish => FISH_INIT,
    }
}

/// Returns the command setup places between the stable markers.
#[must_use]
pub const fn activation_line(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => r#"eval "$(argmax init bash)""#,
        Shell::Zsh => r#"eval "$(argmax init zsh)""#,
        Shell::Fish => "argmax init fish | source",
    }
}

/// Builds a complete LF-terminated setup block for display or a new file.
#[must_use]
pub fn setup_block(shell: Shell) -> String {
    render_block(shell, b"\n", true)
}

/// Suggested shell-config file and manual setup command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellConfigTarget {
    shell: Shell,
    path: PathBuf,
}

impl ShellConfigTarget {
    /// The shell for which this target was selected.
    #[must_use]
    pub const fn shell(&self) -> Shell {
        self.shell
    }

    /// Exact config path suggested to the setup caller.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A source command suitable for a manual setup instruction.
    #[must_use]
    pub const fn activation_line(&self) -> &'static str {
        activation_line(self.shell)
    }
}

/// Selects a config target without reading the environment or filesystem.
///
/// `ZDOTDIR` follows Zsh's exact lexical semantics: only an unset value falls
/// back to `HOME`, an empty value selects `/.zshrc`, and a relative value stays
/// relative to the shell's current directory. An empty or relative
/// `XDG_CONFIG_HOME` is ignored as required by the XDG base-directory
/// specification.
#[must_use]
pub fn suggest_config_target(
    shell: Shell,
    home: &Path,
    zdotdir: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> ShellConfigTarget {
    let nonempty = |path: &&Path| !path.as_os_str().is_empty();
    let path = match shell {
        Shell::Bash => home.join(".bashrc"),
        Shell::Zsh => match zdotdir {
            Some(path) if path.as_os_str().is_empty() => PathBuf::from("/.zshrc"),
            Some(path) => path.join(".zshrc"),
            None => home.join(".zshrc"),
        },
        Shell::Fish => xdg_config_home
            .filter(|path| nonempty(path) && path.is_absolute())
            .map_or_else(|| home.join(".config"), Path::to_path_buf)
            .join("fish")
            .join("config.fish"),
    };
    ShellConfigTarget { shell, path }
}

/// The kind of unmarked integration recognized for migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyStyle {
    /// POSIX-family `eval "$(argmax init SHELL)"` setup.
    Eval,
    /// Fish `argmax init fish | source` setup.
    FishPipeSource,
}

/// One unmarked legacy integration line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyIntegration {
    shell: Shell,
    style: LegacyStyle,
    line: usize,
}

impl LegacyIntegration {
    /// Shell named by the legacy line.
    #[must_use]
    pub const fn shell(self) -> Shell {
        self.shell
    }

    /// Syntax used by the legacy line.
    #[must_use]
    pub const fn style(self) -> LegacyStyle {
        self.style
    }

    /// One-based line number in the inspected file.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }
}

/// Read-only integration facts discovered in one shell config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigInspection {
    has_marked_block: bool,
    legacy_integrations: Vec<LegacyIntegration>,
}

impl ConfigInspection {
    /// Whether exactly one well-formed stable marked block exists.
    #[must_use]
    pub const fn has_marked_block(&self) -> bool {
        self.has_marked_block
    }

    /// Unmarked legacy lines retained for an explicit migration decision.
    #[must_use]
    pub fn legacy_integrations(&self) -> &[LegacyIntegration] {
        &self.legacy_integrations
    }
}

/// Result category for an idempotent config edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditOutcome {
    /// The desired block already exists, or matching legacy setup was retained.
    Unchanged,
    /// A marked block was appended.
    Appended,
    /// A pre-existing marked block was replaced in place.
    Replaced,
}

/// The pure result of editing shell-config bytes.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfigEdit {
    content: Vec<u8>,
    outcome: EditOutcome,
    legacy_integrations: Vec<LegacyIntegration>,
    source_managed_bytes: usize,
    source_unmanaged_bytes: usize,
}

impl fmt::Debug for ConfigEdit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigEdit")
            .field("content_bytes", &self.content.len())
            .field("outcome", &self.outcome)
            .field("legacy_integration_count", &self.legacy_integrations.len())
            .field("source_managed_bytes", &self.source_managed_bytes)
            .field("source_unmanaged_bytes", &self.source_unmanaged_bytes)
            .finish()
    }
}

impl ConfigEdit {
    /// Edited bytes. Unrelated bytes are preserved exactly.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Whether setup changed the content and how.
    #[must_use]
    pub const fn outcome(&self) -> EditOutcome {
        self.outcome
    }

    /// Unmarked legacy lines that were deliberately retained.
    #[must_use]
    pub fn legacy_integrations(&self) -> &[LegacyIntegration] {
        &self.legacy_integrations
    }

    /// Bytes occupied by the one marked block and its line delimiters in the
    /// inspected source.
    #[must_use]
    pub const fn source_managed_bytes(&self) -> usize {
        self.source_managed_bytes
    }

    /// Exact inspected-source bytes outside the one bounded managed span.
    #[must_use]
    pub const fn source_unmanaged_bytes(&self) -> usize {
        self.source_unmanaged_bytes
    }

    /// Consumes the result and returns its edited bytes.
    #[must_use]
    pub fn into_content(self) -> Vec<u8> {
        self.content
    }
}

/// Structural failure found while locating the stable marked block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigEditError {
    /// A second begin marker occurred before the first block ended.
    NestedBeginMarker {
        /// One-based line containing the nested marker.
        line: usize,
    },
    /// A second complete block began after a complete block.
    DuplicateBlock {
        /// One-based line containing the duplicate begin marker.
        line: usize,
    },
    /// An end marker appeared without a preceding begin marker.
    UnexpectedEndMarker {
        /// One-based line containing the marker.
        line: usize,
    },
    /// The file ended before an open block's end marker.
    MissingEndMarker {
        /// One-based line containing the unmatched begin marker.
        line: usize,
    },
}

impl fmt::Display for ConfigEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NestedBeginMarker { line } => {
                write!(formatter, "nested argmax begin marker on line {line}")
            }
            Self::DuplicateBlock { line } => {
                write!(
                    formatter,
                    "duplicate argmax integration block on line {line}"
                )
            }
            Self::UnexpectedEndMarker { line } => {
                write!(
                    formatter,
                    "argmax end marker without a begin marker on line {line}"
                )
            }
            Self::MissingEndMarker { line } => {
                write!(
                    formatter,
                    "argmax begin marker on line {line} has no end marker"
                )
            }
        }
    }
}

impl Error for ConfigEditError {}

/// Inspects stable markers and unmarked legacy integration lines.
///
/// # Errors
///
/// Returns an error for unbalanced, nested, or duplicate stable markers.
pub fn inspect_config(content: &[u8]) -> Result<ConfigInspection, ConfigEditError> {
    let ConfigScan {
        marked_range,
        legacy_integrations,
    } = scan_config(content)?;
    Ok(ConfigInspection {
        has_marked_block: marked_range.is_some(),
        legacy_integrations,
    })
}

/// Adds or replaces one stable setup block without filesystem access.
///
/// Matching legacy setup is reported and left unchanged so that setup never
/// activates a second wrapper. A caller may offer an explicit migration after
/// taking the required backup.
///
/// # Errors
///
/// Returns an error for unbalanced, nested, or duplicate stable markers.
pub fn edit_config(content: &[u8], shell: Shell) -> Result<ConfigEdit, ConfigEditError> {
    let ConfigScan {
        marked_range,
        legacy_integrations,
    } = scan_config(content)?;
    let (source_managed_bytes, source_unmanaged_bytes) =
        source_byte_partition(content, marked_range.as_ref());

    if let Some(range) = marked_range {
        let newline = preferred_newline(content);
        let block = render_block(shell, newline, false).into_bytes();
        if content[range.clone()] == block {
            return Ok(ConfigEdit {
                content: content.to_vec(),
                outcome: EditOutcome::Unchanged,
                legacy_integrations,
                source_managed_bytes,
                source_unmanaged_bytes,
            });
        }

        let mut edited = Vec::with_capacity(content.len() - range.len() + block.len());
        edited.extend_from_slice(&content[..range.start]);
        edited.extend_from_slice(&block);
        edited.extend_from_slice(&content[range.end..]);
        return Ok(ConfigEdit {
            content: edited,
            outcome: EditOutcome::Replaced,
            legacy_integrations,
            source_managed_bytes,
            source_unmanaged_bytes,
        });
    }

    if legacy_integrations
        .iter()
        .any(|integration| integration.shell == shell)
    {
        return Ok(ConfigEdit {
            content: content.to_vec(),
            outcome: EditOutcome::Unchanged,
            legacy_integrations,
            source_managed_bytes,
            source_unmanaged_bytes,
        });
    }

    let newline = preferred_newline(content);
    let mut edited = Vec::with_capacity(content.len() + 128);
    edited.extend_from_slice(content);
    if !content.is_empty() {
        edited.extend_from_slice(newline);
    }
    edited.extend_from_slice(render_block(shell, newline, true).as_bytes());
    Ok(ConfigEdit {
        content: edited,
        outcome: EditOutcome::Appended,
        legacy_integrations,
        source_managed_bytes,
        source_unmanaged_bytes,
    })
}

fn source_byte_partition(content: &[u8], marked_range: Option<&Range<usize>>) -> (usize, usize) {
    let Some(marked_range) = marked_range else {
        return (0, content.len());
    };
    let mut start = marked_range.start;
    if content[..start].ends_with(b"\r\n") {
        start -= 2;
    } else if content[..start].ends_with(b"\n") {
        start -= 1;
    }
    let mut end = marked_range.end;
    if content[end..].starts_with(b"\r\n") {
        end += 2;
    } else if content[end..].starts_with(b"\n") {
        end += 1;
    }
    let managed = end - start;
    (managed, content.len() - managed)
}

#[derive(Clone, Copy)]
struct Line<'a> {
    number: usize,
    start: usize,
    content: &'a [u8],
    content_end: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Marker {
    Begin,
    End,
}

struct LineIter<'a> {
    content: &'a [u8],
    start: usize,
    number: usize,
}

impl<'a> LineIter<'a> {
    const fn new(content: &'a [u8]) -> Self {
        Self {
            content,
            start: 0,
            number: 1,
        }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = Line<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.content.len() {
            return None;
        }
        let start = self.start;
        let newline = self.content[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| start + offset);
        let raw_end = newline.unwrap_or(self.content.len());
        let content_end = if raw_end > start && self.content[raw_end - 1] == b'\r' {
            raw_end - 1
        } else {
            raw_end
        };
        let line = Line {
            number: self.number,
            start,
            content: &self.content[start..content_end],
            content_end,
        };
        if let Some(newline) = newline {
            self.start = newline + 1;
            self.number += 1;
        } else {
            self.start = self.content.len();
        }
        Some(line)
    }
}

fn marker(line: &[u8]) -> Option<Marker> {
    match line {
        bytes if bytes == BEGIN_MARKER.as_bytes() => Some(Marker::Begin),
        bytes if bytes == END_MARKER.as_bytes() => Some(Marker::End),
        _ => None,
    }
}

struct ConfigScan {
    marked_range: Option<Range<usize>>,
    legacy_integrations: Vec<LegacyIntegration>,
}

fn scan_config(content: &[u8]) -> Result<ConfigScan, ConfigEditError> {
    let mut open: Option<(usize, usize)> = None;
    let mut marked_range = None;
    let mut legacy_integrations = Vec::new();
    for line in LineIter::new(content) {
        match marker(line.content) {
            Some(Marker::Begin) if open.is_some() => {
                return Err(ConfigEditError::NestedBeginMarker { line: line.number });
            }
            Some(Marker::Begin) if marked_range.is_some() => {
                return Err(ConfigEditError::DuplicateBlock { line: line.number });
            }
            Some(Marker::Begin) => open = Some((line.number, line.start)),
            Some(Marker::End) => {
                let Some((_, begin_start)) = open.take() else {
                    return Err(ConfigEditError::UnexpectedEndMarker { line: line.number });
                };
                marked_range = Some(begin_start..line.content_end);
            }
            None if open.is_none() => {
                if let Some((shell, style)) = legacy_line(line.content) {
                    legacy_integrations.push(LegacyIntegration {
                        shell,
                        style,
                        line: line.number,
                    });
                }
            }
            None => {}
        }
    }
    if let Some((line, _)) = open {
        return Err(ConfigEditError::MissingEndMarker { line });
    }
    Ok(ConfigScan {
        marked_range,
        legacy_integrations,
    })
}

fn legacy_line(line: &[u8]) -> Option<(Shell, LegacyStyle)> {
    let text = std::str::from_utf8(line).ok()?.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    for (shell, pattern) in [
        (Shell::Bash, r#"eval "$(argmax init bash)""#),
        (Shell::Bash, r#"eval "$(command argmax init bash)""#),
        (Shell::Zsh, r#"eval "$(argmax init zsh)""#),
        (Shell::Zsh, r#"eval "$(command argmax init zsh)""#),
        (Shell::Fish, r#"eval "$(argmax init fish)""#),
        (Shell::Fish, r#"eval "$(command argmax init fish)""#),
    ] {
        if normalized_line_matches(text, pattern) {
            return Some((shell, LegacyStyle::Eval));
        }
    }
    if normalized_line_matches(text, "argmax init fish | source")
        || normalized_line_matches(text, "command argmax init fish | source")
    {
        return Some((Shell::Fish, LegacyStyle::FishPipeSource));
    }
    None
}

fn normalized_line_matches(text: &str, pattern: &str) -> bool {
    let mut actual = text.split_whitespace();
    if !pattern.split(' ').all(|part| actual.next() == Some(part)) {
        return false;
    }
    actual.next().is_none_or(|part| part.starts_with('#'))
}

fn preferred_newline(content: &[u8]) -> &'static [u8] {
    let Some(index) = content.iter().position(|byte| *byte == b'\n') else {
        return b"\n";
    };
    if index > 0 && content[index - 1] == b'\r' {
        b"\r\n"
    } else {
        b"\n"
    }
}

fn render_block(shell: Shell, newline: &[u8], terminal_newline: bool) -> String {
    let newline = std::str::from_utf8(newline).expect("newlines are valid UTF-8");
    let mut block = [BEGIN_MARKER, activation_line(shell), END_MARKER].join(newline);
    if terminal_newline {
        block.push_str(newline);
    }
    block
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn init_scripts_are_sourceable_guards_without_argument_mutation() {
        let cases = BTreeMap::from([
            (Shell::Bash, ("$- == *i*", "exec argmax --shell bash")),
            (
                Shell::Fish,
                ("status is-interactive", "exec argmax --shell fish"),
            ),
            (Shell::Zsh, ("-o interactive", "exec argmax --shell zsh")),
        ]);

        for (shell, (interactive_guard, wrapper)) in cases {
            let script = init_script(shell);
            assert!(script.ends_with('\n'));
            assert!(script.contains(interactive_guard));
            assert!(script.contains(wrapper));
            assert!(script.contains(SESSION_MARKER_ENV));
            assert!(script.contains(SESSION_OWNER_PID_ENV));
            assert!(script.contains("command-start"));
            assert!(script.contains("command-stop:"));
            assert!(script.contains("buffer:"));
            assert!(script.contains("prompt-ready"));
            assert!(script.contains("capability:"));
            assert!(!script.contains("set --"));
            assert!(!script.contains("shift"));
            assert!(!script.contains("$@"));
            assert!(!script.contains("command ps -o comm="));
        }

        assert!(!init_script(Shell::Bash).contains("$BASH_COMMAND"));
        assert!(!init_script(Shell::Bash).contains("command-start:"));
        assert!(!init_script(Shell::Bash).contains("__ARGMAX_BASH_SUBMITTED"));
        assert!(!init_script(Shell::Bash).contains("__ARGMAX_BASH_READY"));
        assert!(init_script(Shell::Bash).contains("__ARGMAX_BASH_COMMAND_ACTIVE"));
        assert!(init_script(Shell::Bash).contains("command-start-unknown"));
        assert!(init_script(Shell::Bash).contains("BASH_VERSINFO[1] < 4"));
        assert!(init_script(Shell::Bash).contains("probe-buffer:$argmax_unit:"));
        assert!(
            init_script(Shell::Bash)
                .contains("PS0=${PS0-}'${__ARGMAX_BASH_COMMAND_ACTIVE:=}$(__argmax_preexec)'")
        );
        assert!(!init_script(Shell::Bash).contains("__ARGMAX_BASH_PS0"));
        assert!(init_script(Shell::Zsh).contains("__ARGMAX_ZSH_COMMAND_ACTIVE"));
        assert!(init_script(Shell::Zsh).contains("[[ -o multibyte ]]"));
        assert!(init_script(Shell::Zsh).contains("probe-buffer:$argmax_unit:"));
        assert!(init_script(Shell::Fish).contains("fish_posterror"));
        assert!(init_script(Shell::Fish).contains("string collect -N"));
        assert!(init_script(Shell::Fish).contains("probe-buffer:f:"));
        assert_eq!(MAX_SYNC_EVENT_CHARACTERS, 16_384);
        assert!(cases_all_contain("16384"));
    }

    #[test]
    fn reports_shell_adapter_capabilities_truthfully() {
        assert_eq!(SYNC_PROBE_SEQUENCE, b"\x1b[argmax-sync~");
        assert_eq!(
            integration_capabilities(Shell::Bash),
            IntegrationCapabilities {
                buffer_sync: BufferSyncAdapter::ReservedProbe,
                command_text: CommandTextAdapter::Unavailable,
            }
        );
        assert_eq!(
            integration_capabilities(Shell::Zsh),
            IntegrationCapabilities {
                buffer_sync: BufferSyncAdapter::ReservedProbe,
                command_text: CommandTextAdapter::ExactPreexec,
            }
        );
        assert_eq!(
            integration_capabilities(Shell::Fish),
            IntegrationCapabilities {
                buffer_sync: BufferSyncAdapter::ReservedProbe,
                command_text: CommandTextAdapter::ExactPreexec,
            }
        );
    }

    #[test]
    fn adapters_verify_ownership_and_rollback_only_attempted_registration() {
        let bash = init_script(Shell::Bash);
        assert!(bash.contains("argmax-owned-bash-v1"));
        assert!(bash.contains("declare -f __argmax_sync"));
        assert!(bash.contains("builtin unset -f __argmax_emit"));
        assert!(bash.contains("__argmax_probe_is_unbound vi-command"));
        assert!(bash.contains("argmax_prompt_attempted"));
        assert!(bash.contains("argmax_ps0_attempted"));
        assert!(bash.contains("argmax_vi_command_attempted"));

        let zsh = init_script(Shell::Zsh);
        assert!(zsh.contains("argmax-owned-zsh-v1"));
        assert!(zsh.contains("__argmax_zsh_bound_maps"));
        assert!(zsh.contains("zle -D __argmax_sync"));
        assert!(zsh.contains("bindkey -M \"$__argmax_zsh_map\""));

        let fish = init_script(Shell::Fish);
        assert!(fish.contains("argmax-owned-fish-v1"));
        assert!(fish.contains("functions $argmax_function |"));
        assert!(fish.contains("argmax_registered_modes"));
        assert!(fish.contains("for argmax_mode in $argmax_registered_modes"));
    }

    fn cases_all_contain(needle: &str) -> bool {
        [Shell::Bash, Shell::Zsh, Shell::Fish]
            .into_iter()
            .all(|shell| init_script(shell).contains(needle))
    }

    #[test]
    fn installed_shells_accept_generated_script_syntax() {
        for (program, argument, shell) in [
            ("bash", "-n", Shell::Bash),
            ("zsh", "-n", Shell::Zsh),
            ("fish", "--no-execute", Shell::Fish),
        ] {
            let mut command = Command::new(program);
            command.arg(argument);
            let mut child = match command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("failed to start {program}: {error}"),
            };
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(init_script(shell).as_bytes())
                .expect("write script");
            let output = child.wait_with_output().expect("wait for shell parser");
            assert!(
                output.status.success(),
                "{program} rejected generated integration:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn bash_script_passes_shellcheck_when_available() {
        let mut child = match Command::new("shellcheck")
            .args(["--shell", "bash", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("failed to start ShellCheck: {error}"),
        };
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(init_script(Shell::Bash).as_bytes())
            .expect("write Bash integration");
        let output = child.wait_with_output().expect("wait for ShellCheck");
        assert!(
            output.status.success(),
            "ShellCheck rejected Bash integration:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    static HARNESS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct HarnessDirectory(PathBuf);

    impl HarnessDirectory {
        fn create(shell: Shell) -> Self {
            let sequence = HARNESS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-{}-harness-{}-{sequence}",
                shell.as_str(),
                std::process::id()
            ));
            fs::create_dir(&path).expect("create shell harness directory");
            Self(path)
        }
    }

    impl Drop for HarnessDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn shell_is_available(program: &str) -> bool {
        match Command::new(program).arg("--version").output() {
            Ok(output) => output.status.success(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("failed to inspect {program}: {error}"),
        }
    }

    fn expect_is_available() -> bool {
        match Command::new("expect").arg("-v").output() {
            Ok(output) => output.status.success(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("failed to inspect expect: {error}"),
        }
    }

    fn live_shell_events(shell: Shell) -> Option<Vec<Vec<u8>>> {
        let program = shell.as_str();
        if !expect_is_available() || !shell_is_available(program) {
            return None;
        }
        if shell == Shell::Bash
            && !Command::new(program)
                .args([
                    "-c",
                    "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))",
                ])
                .status()
                .expect("inspect Bash version")
                .success()
        {
            return None;
        }

        let directory = HarnessDirectory::create(shell);
        let init_path = directory.0.join("init");
        let events_path = directory.0.join("events");
        fs::write(&init_path, init_script(shell)).expect("write shell init");

        let shell_arguments = match shell {
            Shell::Bash => "--noprofile --norc -i",
            Shell::Zsh => "-df",
            Shell::Fish => return None,
        };
        let expect_program = r#"
          set timeout 10
          log_user 0
          spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
          send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 2 }
            eof { exit 3 }
          }
          send -- "source \"$env(ARGMAX_TEST_INIT)\"\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 6 }
            eof { exit 7 }
          }
          send -- "echo hi"
          send -- "\033\[argmax-sync~"
          after 100
          send -- "\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 4 }
            eof { exit 5 }
          }
          send -- "exit\r"
          expect eof
        "#;
        let output = Command::new("expect")
            .args(["-c", expect_program])
            .env("ARGMAX_PRIVATE_SESSION", "1")
            .env("ARGMAX_EVENT_FD", "3")
            .env("ARGMAX_TEST_EVENTS", &events_path)
            .env("ARGMAX_TEST_INIT", &init_path)
            .env("ARGMAX_TEST_SHELL", program)
            .env("ARGMAX_TEST_ARGS", shell_arguments)
            .output()
            .expect("run shell harness");
        let events = fs::read(&events_path).unwrap_or_default();
        assert!(
            output.status.success(),
            "{program} PTY harness failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Some(
            events
                .split(|byte| *byte == 0)
                .filter(|frame| !frame.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        )
    }

    fn collision_harness(shell: Shell) -> Option<Vec<Vec<u8>>> {
        let program = shell.as_str();
        if !expect_is_available() || !shell_is_available(program) {
            return None;
        }
        if shell == Shell::Bash
            && !Command::new(program)
                .args([
                    "-c",
                    "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))",
                ])
                .status()
                .expect("inspect Bash version")
                .success()
        {
            return None;
        }

        let directory = HarnessDirectory::create(shell);
        let init_path = directory.0.join("init");
        let events_path = directory.0.join("events");
        let sentinel_path = directory.0.join("preserved");
        fs::write(&init_path, init_script(shell)).expect("write shell init");
        let shell_arguments = match shell {
            Shell::Bash => "--noprofile --norc -i",
            Shell::Zsh => "-df",
            Shell::Fish => "--no-config --interactive",
        };
        let expect_program = r#"
          set timeout 10
          log_user 0
          spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
          if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
            send -- "function fish_prompt; printf 'ARGMAX\\x3e '; end; function __argmax_sync; set -l user_owned 1; end; set -g __ARGMAX_FISH_INSTALLED fake; source \"$env(ARGMAX_TEST_INIT)\"; functions __argmax_sync | string match -q '*user_owned*'; and printf preserved > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          } elseif {$env(ARGMAX_TEST_SHELL) eq "zsh"} {
            send -- "PS1='ARGMAX''> '; __argmax_sync(){ : user-owned; }; __ARGMAX_ZSH_HOOKS=fake; source \"$env(ARGMAX_TEST_INIT)\"; functions __argmax_sync | grep -q user-owned && printf preserved > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          } else {
            send -- "PS1='ARGMAX''> '; __argmax_sync(){ : user-owned; }; __ARGMAX_BASH_HOOKS=fake; source \"$env(ARGMAX_TEST_INIT)\"; declare -f __argmax_sync | grep -q user-owned && printf preserved > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          }
          expect {
            "ARGMAX> " {}
            timeout { exit 2 }
            eof { exit 3 }
          }
          send -- "exit\r"
          expect eof
        "#;
        let output = Command::new("expect")
            .args(["-c", expect_program])
            .env("ARGMAX_PRIVATE_SESSION", "1")
            .env("ARGMAX_EVENT_FD", "3")
            .env("ARGMAX_TEST_EVENTS", &events_path)
            .env("ARGMAX_TEST_INIT", &init_path)
            .env("ARGMAX_TEST_SENTINEL", &sentinel_path)
            .env("ARGMAX_TEST_SHELL", program)
            .env("ARGMAX_TEST_ARGS", shell_arguments)
            .output()
            .expect("run collision harness");
        let events = fs::read(&events_path).unwrap_or_default();
        let preserved = fs::read(&sentinel_path).unwrap_or_default();
        assert!(
            output.status.success(),
            "{program} collision harness failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(preserved, b"preserved", "{program} replaced a user helper");
        Some(
            events
                .split(|byte| *byte == 0)
                .filter(|frame| !frame.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        )
    }

    fn inherited_session_harness(shell: Shell) -> Option<()> {
        let program = shell.as_str();
        if !expect_is_available() || !shell_is_available(program) {
            return None;
        }

        let directory = HarnessDirectory::create(shell);
        let init_path = directory.0.join("init");
        let events_path = directory.0.join("events");
        let sentinel_path = directory.0.join("cleared");
        fs::write(&init_path, init_script(shell)).expect("write shell init");
        let shell_arguments = match shell {
            Shell::Bash => "--noprofile --norc -i",
            Shell::Zsh => "-df",
            Shell::Fish => "--no-config --interactive",
        };
        let expect_program = r#"
          set timeout 10
          log_user 0
          spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec $ARGMAX_TEST_SHELL $ARGMAX_TEST_ARGS}
          if {$env(ARGMAX_TEST_SHELL) eq "fish"} {
            send -- "function fish_prompt; printf 'ARGMAX\\x3e '; end; source \"$env(ARGMAX_TEST_INIT)\"; not set -q ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL ARGMAX_SESSION_OWNER_PID; and not functions -q __argmax_emit; and printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          } elseif {$env(ARGMAX_TEST_SHELL) eq "zsh"} {
            send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"; test -z \"\${ARGMAX_PRIVATE_SESSION+x}\" && test -z \"\${ARGMAX_EVENT_FD+x}\" && test -z \"\${ARGMAX_ACTIVE_SHELL+x}\" && test -z \"\${ARGMAX_SESSION_OWNER_PID+x}\" && ! functions __argmax_emit >/dev/null 2>&1 && printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          } else {
            send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_INIT)\"; test -z \"\${ARGMAX_PRIVATE_SESSION+x}\" && test -z \"\${ARGMAX_EVENT_FD+x}\" && test -z \"\${ARGMAX_ACTIVE_SHELL+x}\" && test -z \"\${ARGMAX_SESSION_OWNER_PID+x}\" && ! declare -F __argmax_emit >/dev/null && printf cleared > \"$env(ARGMAX_TEST_SENTINEL)\"\r"
          }
          expect {
            "ARGMAX> " {}
            timeout { exit 2 }
            eof { exit 3 }
          }
          send -- "exit\r"
          expect eof
        "#;
        let output = Command::new("expect")
            .args(["-c", expect_program])
            .env("ARGMAX_PRIVATE_SESSION", "1")
            .env("ARGMAX_EVENT_FD", "3")
            .env("ARGMAX_ACTIVE_SHELL", program)
            .env("ARGMAX_SESSION_OWNER_PID", "1")
            .env("ARGMAX_TEST_EVENTS", &events_path)
            .env("ARGMAX_TEST_INIT", &init_path)
            .env("ARGMAX_TEST_SENTINEL", &sentinel_path)
            .env("ARGMAX_TEST_SHELL", program)
            .env("ARGMAX_TEST_ARGS", shell_arguments)
            .output()
            .expect("run inherited session harness");
        assert!(
            output.status.success(),
            "{program} inherited-session harness failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(&sentinel_path).unwrap_or_default(),
            b"cleared",
            "{program} retained inherited session authority"
        );
        assert_eq!(
            fs::read(&events_path).unwrap_or_default(),
            b"",
            "{program} wrote to the inherited event channel"
        );
        Some(())
    }

    fn bash_readonly_harness(readonly_variable: &str) -> Option<Vec<Vec<u8>>> {
        if !expect_is_available() || !shell_is_available("bash") {
            return None;
        }
        if !Command::new("bash")
            .args([
                "-c",
                "(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))",
            ])
            .status()
            .expect("inspect Bash version")
            .success()
        {
            return None;
        }

        let directory = HarnessDirectory::create(Shell::Bash);
        let init_path = directory.0.join("init");
        let check_path = directory.0.join("check");
        let events_path = directory.0.join("events");
        let sentinel_path = directory.0.join("preserved");
        fs::write(&init_path, init_script(Shell::Bash)).expect("write Bash init");
        fs::write(
            &check_path,
            r#"PS0=user-ps0
PROMPT_COMMAND=:
readonly "$ARGMAX_TEST_READONLY"
source "$ARGMAX_TEST_INIT"
if test "$PS0" = user-ps0 &&
    test "$PROMPT_COMMAND" = : &&
    test -z "${__ARGMAX_BASH_HOOKS+x}" &&
    test -z "${__ARGMAX_BASH_CAPABILITY+x}" &&
    test -z "${__ARGMAX_BASH_COMMAND_ACTIVE+x}" &&
    test -z "${__ARGMAX_BASH_PROBE+x}" &&
    test -z "${__ARGMAX_BASH_PROBE_NONCE+x}" &&
    ! declare -F __argmax_emit >/dev/null &&
    ! declare -F __argmax_preexec >/dev/null &&
    ! declare -F __argmax_precmd >/dev/null &&
    ! declare -F __argmax_sync >/dev/null &&
    ! declare -F __argmax_probe_is_unbound >/dev/null &&
    ! declare -F __argmax_install >/dev/null &&
    ! builtin bind -m emacs-standard -X 2>/dev/null |
      command grep -q argmax-sync &&
    ! builtin bind -m vi-insert -X 2>/dev/null |
      command grep -q argmax-sync &&
    ! builtin bind -m vi-command -X 2>/dev/null |
      command grep -q argmax-sync; then
  printf preserved > "$ARGMAX_TEST_SENTINEL"
fi
"#,
        )
        .expect("write Bash readonly check");
        let expect_program = r#"
          set timeout 10
          log_user 0
          spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec bash --noprofile --norc -i}
          after 100
          send -- "PS1='ARGMAX''> '; source \"$env(ARGMAX_TEST_CHECK)\"\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 2 }
            eof { exit 3 }
          }
          send -- "exit\r"
          expect eof
        "#;
        let output = Command::new("expect")
            .args(["-c", expect_program])
            .env("ARGMAX_PRIVATE_SESSION", "1")
            .env("ARGMAX_EVENT_FD", "3")
            .env("ARGMAX_TEST_CHECK", &check_path)
            .env("ARGMAX_TEST_EVENTS", &events_path)
            .env("ARGMAX_TEST_INIT", &init_path)
            .env("ARGMAX_TEST_READONLY", readonly_variable)
            .env("ARGMAX_TEST_SENTINEL", &sentinel_path)
            .output()
            .expect("run Bash readonly harness");
        assert!(
            output.status.success(),
            "Bash readonly-{readonly_variable} harness failed with {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(&sentinel_path).unwrap_or_default(),
            b"preserved",
            "Bash left partial installation after readonly {readonly_variable}"
        );
        let events = fs::read(&events_path).unwrap_or_default();
        Some(
            events
                .split(|byte| *byte == 0)
                .filter(|frame| !frame.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        )
    }

    fn bash_prompt_lifecycle_events() -> Option<Vec<Vec<u8>>> {
        if !expect_is_available() || !shell_is_available("bash") {
            return None;
        }

        let directory = HarnessDirectory::create(Shell::Bash);
        let init_path = directory.0.join("init");
        let events_path = directory.0.join("events");
        let statuses_path = directory.0.join("statuses");
        fs::write(&init_path, init_script(Shell::Bash)).expect("write Bash init");
        let expect_program = r#"
          set timeout 10
          log_user 0
          spawn sh -c {exec 3>"$ARGMAX_TEST_EVENTS"; exec bash --noprofile --norc -i}
          send -- "PS1='ARGMAX''> '; PROMPT_COMMAND='printf \"%s\\n\" \"\$?\" >> \"$env(ARGMAX_TEST_STATUSES)\"'; source \"$env(ARGMAX_TEST_INIT)\"\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 2 }
            eof { exit 3 }
          }
          send -- "\003"
          expect {
            "ARGMAX> " {}
            timeout { exit 4 }
            eof { exit 5 }
          }
          send -- ")\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 6 }
            eof { exit 7 }
          }
          send -- "true\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 8 }
            eof { exit 9 }
          }
          send -- "false\r"
          expect {
            "ARGMAX> " {}
            timeout { exit 10 }
            eof { exit 11 }
          }
          send -- "exit\r"
          expect eof
        "#;
        let output = Command::new("expect")
            .args(["-c", expect_program])
            .env("ARGMAX_PRIVATE_SESSION", "1")
            .env("ARGMAX_EVENT_FD", "3")
            .env("ARGMAX_TEST_EVENTS", &events_path)
            .env("ARGMAX_TEST_INIT", &init_path)
            .env("ARGMAX_TEST_STATUSES", &statuses_path)
            .output()
            .expect("run Bash prompt lifecycle harness");
        assert!(
            output.status.success(),
            "Bash prompt lifecycle harness failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let statuses = fs::read_to_string(&statuses_path).unwrap_or_default();
        assert_eq!(
            statuses.lines().next_back(),
            Some("1"),
            "Bash changed the status seen by an existing prompt hook: {statuses:?}"
        );
        let events = fs::read(&events_path).unwrap_or_default();
        Some(
            events
                .split(|byte| *byte == 0)
                .filter(|frame| !frame.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        )
    }

    fn assert_bash_lifecycle_is_paired(events: &[Vec<u8>]) {
        let mut active = false;
        let mut stops = 0;
        for frame in events {
            if frame == b"command-start-unknown" {
                assert!(!active, "duplicate Bash command start in {events:?}");
                active = true;
            } else if frame.starts_with(b"command-stop:") {
                assert!(active, "Bash command stop without a start in {events:?}");
                active = false;
                stops += 1;
            }
        }
        assert!(stops > 0, "Bash harness observed no completed command");
    }

    #[test]
    fn bash_live_harness_reports_correlated_probe_and_unknown_start() {
        let Some(events) = live_shell_events(Shell::Bash) else {
            return;
        };
        assert_bash_lifecycle_is_paired(&events);
        assert!(
            events
                .iter()
                .filter(|frame| frame.as_slice() == b"capability:sync-probe:0")
                .count()
                >= 2
        );
        assert!(events.iter().any(|frame| {
            frame.starts_with(b"probe-buffer:")
                && frame.windows(b":1:".len()).any(|window| window == b":1:")
                && frame.ends_with(b":echo hi")
        }));
        assert!(events.iter().any(|frame| frame == b"command-start-unknown"));
        assert!(!events.iter().any(|frame| frame == b"command-start:echo hi"));
        assert!(events.iter().any(|frame| frame == b"command-stop:0"));
    }

    #[test]
    fn bash_empty_prompt_interrupt_and_syntax_error_never_emit_orphan_stop() {
        let Some(events) = bash_prompt_lifecycle_events() else {
            return;
        };
        assert_bash_lifecycle_is_paired(&events);
    }

    #[test]
    fn zsh_live_harness_reports_exact_preexec_and_correlated_probe() {
        let Some(events) = live_shell_events(Shell::Zsh) else {
            return;
        };
        assert!(
            events
                .iter()
                .filter(|frame| frame.as_slice() == b"capability:sync-probe:0")
                .count()
                >= 2
        );
        assert!(
            events.iter().any(|frame| {
                frame.starts_with(b"probe-buffer:") && frame.ends_with(b":echo hi")
            })
        );
        assert!(events.iter().any(|frame| frame == b"command-start:echo hi"));
        assert!(events.iter().any(|frame| frame == b"command-stop:0"));
    }

    #[test]
    fn preexisting_markers_and_helpers_fail_closed_without_overwrite() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let Some(events) = collision_harness(shell) else {
                continue;
            };
            assert!(
                events
                    .iter()
                    .any(|frame| frame == b"capability:unavailable"),
                "{} did not report the collision",
                shell.as_str()
            );
            assert!(
                !events.iter().any(|frame| {
                    frame.starts_with(b"capability:sync-probe:")
                        || frame == b"capability:native-buffer"
                }),
                "{} claimed authority after a collision",
                shell.as_str()
            );
        }
    }

    #[test]
    fn inherited_sessions_cannot_claim_parent_event_channels() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let _ = inherited_session_harness(shell);
        }
    }

    #[test]
    fn bash_readonly_prompt_variables_fail_before_any_mutation() {
        for variable in ["PS0", "PROMPT_COMMAND"] {
            let Some(events) = bash_readonly_harness(variable) else {
                return;
            };
            assert_eq!(
                events,
                [b"capability:unavailable".to_vec()],
                "readonly {variable} emitted unexpected events"
            );
        }
    }

    #[test]
    fn suggests_targets_from_shell_specific_config_roots() {
        let home = Path::new("/Users/troy");
        let zdotdir = Path::new("/Users/troy/Greendale/zsh");
        let xdg = Path::new("/Users/troy/Greendale/config");
        let cases = BTreeMap::from([
            (Shell::Bash, PathBuf::from("/Users/troy/.bashrc")),
            (
                Shell::Fish,
                PathBuf::from("/Users/troy/Greendale/config/fish/config.fish"),
            ),
            (
                Shell::Zsh,
                PathBuf::from("/Users/troy/Greendale/zsh/.zshrc"),
            ),
        ]);

        for (shell, want) in cases {
            let target = suggest_config_target(shell, home, Some(zdotdir), Some(xdg));
            assert_eq!(target.path(), want);
            assert_eq!(target.shell(), shell);
            assert_eq!(target.activation_line(), activation_line(shell));
        }

        assert_eq!(
            suggest_config_target(Shell::Fish, home, None, None).path(),
            Path::new("/Users/troy/.config/fish/config.fish")
        );
        assert_eq!(
            suggest_config_target(Shell::Fish, home, None, Some(Path::new("relative-xdg")),).path(),
            Path::new("/Users/troy/.config/fish/config.fish")
        );
        assert_eq!(
            suggest_config_target(Shell::Zsh, home, Some(Path::new("")), None).path(),
            Path::new("/.zshrc")
        );
        assert_eq!(
            suggest_config_target(Shell::Zsh, home, Some(Path::new("relative-zdotdir")), None,)
                .path(),
            Path::new("relative-zdotdir/.zshrc")
        );
    }

    #[test]
    fn appends_once_and_preserves_existing_newline_style() {
        let original = b"# Greendale shell\r\nexport DEAN=Pelton\r\n";
        let first = edit_config(original, Shell::Fish).unwrap();
        assert_eq!(first.outcome(), EditOutcome::Appended);
        assert!(first.content().starts_with(original));
        for (index, byte) in first.content().iter().enumerate() {
            if *byte == b'\n' {
                assert_eq!(first.content().get(index.wrapping_sub(1)), Some(&b'\r'));
            }
        }
        assert!(
            String::from_utf8_lossy(first.content()).contains("argmax init fish | source\r\n# <<<")
        );

        let second = edit_config(first.content(), Shell::Fish).unwrap();
        assert_eq!(second.outcome(), EditOutcome::Unchanged);
        assert_eq!(second.content(), first.content());
    }

    #[test]
    fn replaces_only_the_marked_bytes() {
        let original =
            format!("# Troy Barnes\n{BEGIN_MARKER}\nold hook\n{END_MARKER}\n# Greendale\n");
        let edit = edit_config(original.as_bytes(), Shell::Zsh).unwrap();

        assert_eq!(edit.outcome(), EditOutcome::Replaced);
        assert!(edit.content().starts_with(b"# Troy Barnes\n"));
        assert!(edit.content().ends_with(b"\n# Greendale\n"));
        assert!(String::from_utf8_lossy(edit.content()).contains(r#"eval "$(argmax init zsh)""#));
    }

    #[test]
    fn reports_exact_source_partitions_for_every_edit_outcome() {
        let appended_source = b"unmanaged";
        let appended = edit_config(appended_source, Shell::Bash).unwrap();
        assert_eq!(appended.outcome(), EditOutcome::Appended);
        assert_eq!(appended.source_managed_bytes(), 0);
        assert_eq!(appended.source_unmanaged_bytes(), appended_source.len());

        let prefix = b"prefix";
        let suffix = b"suffix";
        let wrong_block = render_block(Shell::Zsh, b"\n", false);
        let mut replaced_source = prefix.to_vec();
        replaced_source.push(b'\n');
        replaced_source.extend_from_slice(wrong_block.as_bytes());
        replaced_source.push(b'\n');
        replaced_source.extend_from_slice(suffix);
        let replaced = edit_config(&replaced_source, Shell::Bash).unwrap();
        assert_eq!(replaced.outcome(), EditOutcome::Replaced);
        assert_eq!(replaced.source_managed_bytes(), wrong_block.len() + 2);
        assert_eq!(
            replaced.source_unmanaged_bytes(),
            prefix.len() + suffix.len()
        );

        let desired_block = render_block(Shell::Bash, b"\n", false);
        let mut unchanged_source = prefix.to_vec();
        unchanged_source.push(b'\n');
        unchanged_source.extend_from_slice(desired_block.as_bytes());
        unchanged_source.push(b'\n');
        unchanged_source.extend_from_slice(suffix);
        let unchanged = edit_config(&unchanged_source, Shell::Bash).unwrap();
        assert_eq!(unchanged.outcome(), EditOutcome::Unchanged);
        assert_eq!(unchanged.source_managed_bytes(), desired_block.len() + 2);
        assert_eq!(
            unchanged.source_unmanaged_bytes(),
            prefix.len() + suffix.len()
        );

        let legacy_source = b"eval \"$(argmax init bash)\"\n";
        let legacy = edit_config(legacy_source, Shell::Bash).unwrap();
        assert_eq!(legacy.outcome(), EditOutcome::Unchanged);
        assert_eq!(legacy.source_managed_bytes(), 0);
        assert_eq!(legacy.source_unmanaged_bytes(), legacy_source.len());

        for (source, edit) in [
            (appended_source.as_slice(), appended),
            (replaced_source.as_slice(), replaced),
            (unchanged_source.as_slice(), unchanged),
            (legacy_source.as_slice(), legacy),
        ] {
            assert_eq!(
                edit.source_managed_bytes() + edit.source_unmanaged_bytes(),
                source.len()
            );
        }
    }

    #[test]
    fn reports_and_retains_unmarked_legacy_integrations() {
        let cases = BTreeMap::from([
            (
                Shell::Bash,
                ("eval \"$(argmax init bash)\"\n", LegacyStyle::Eval),
            ),
            (
                Shell::Fish,
                (
                    "  argmax   init fish | source\n",
                    LegacyStyle::FishPipeSource,
                ),
            ),
            (
                Shell::Zsh,
                ("eval \"$(command argmax init zsh)\"\n", LegacyStyle::Eval),
            ),
        ]);

        for (shell, (content, style)) in cases {
            let inspection = inspect_config(content.as_bytes()).unwrap();
            assert!(!inspection.has_marked_block());
            assert_eq!(
                inspection.legacy_integrations(),
                &[LegacyIntegration {
                    shell,
                    style,
                    line: 1,
                }]
            );

            let edit = edit_config(content.as_bytes(), shell).unwrap();
            assert_eq!(edit.outcome(), EditOutcome::Unchanged);
            assert_eq!(edit.content(), content.as_bytes());
        }
    }

    #[test]
    fn recognizes_active_legacy_hooks_with_trailing_comments_only() {
        for (shell, content) in [
            (
                Shell::Bash,
                "eval \"$(argmax init bash)\" # installed by argmax\n",
            ),
            (
                Shell::Zsh,
                "eval \"$(command argmax init zsh)\" # legacy hook\n",
            ),
            (Shell::Fish, "argmax init fish | source # legacy hook\n"),
        ] {
            let inspection = inspect_config(content.as_bytes()).unwrap();
            assert_eq!(inspection.legacy_integrations().len(), 1);
            let edit = edit_config(content.as_bytes(), shell).unwrap();
            assert_eq!(edit.outcome(), EditOutcome::Unchanged);
            assert_eq!(edit.content(), content.as_bytes());
        }

        let commented_out = "# eval \"$(argmax init bash)\"\n";
        let inspection = inspect_config(commented_out.as_bytes()).unwrap();
        assert!(inspection.legacy_integrations().is_empty());
        assert_eq!(
            edit_config(commented_out.as_bytes(), Shell::Bash)
                .unwrap()
                .outcome(),
            EditOutcome::Appended
        );
    }

    #[test]
    fn ignores_legacy_syntax_inside_a_managed_block() {
        let content = setup_block(Shell::Bash);
        let inspection = inspect_config(content.as_bytes()).unwrap();

        assert!(inspection.has_marked_block());
        assert!(inspection.legacy_integrations().is_empty());
    }

    #[test]
    fn rejects_malformed_duplicate_and_nested_markers() {
        let cases = BTreeMap::from([
            (
                "duplicate",
                (
                    format!("{BEGIN_MARKER}\na\n{END_MARKER}\n{BEGIN_MARKER}\nb\n{END_MARKER}\n"),
                    ConfigEditError::DuplicateBlock { line: 4 },
                ),
            ),
            (
                "missing end",
                (
                    format!("# Greendale\n{BEGIN_MARKER}\na\n"),
                    ConfigEditError::MissingEndMarker { line: 2 },
                ),
            ),
            (
                "nested",
                (
                    format!("{BEGIN_MARKER}\na\n{BEGIN_MARKER}\n{END_MARKER}\n"),
                    ConfigEditError::NestedBeginMarker { line: 3 },
                ),
            ),
            (
                "unexpected end",
                (
                    format!("# Greendale\n{END_MARKER}\n"),
                    ConfigEditError::UnexpectedEndMarker { line: 2 },
                ),
            ),
        ]);

        for (_name, (content, want)) in cases {
            assert_eq!(inspect_config(content.as_bytes()), Err(want));
        }
    }

    #[test]
    fn preserves_non_utf8_unrelated_bytes() {
        let original = b"# Troy \xff Barnes\n";
        let edit = edit_config(original, Shell::Bash).unwrap();

        assert_eq!(&edit.content()[..original.len()], original);
        assert_eq!(edit.outcome(), EditOutcome::Appended);
    }

    #[test]
    fn config_edit_debug_redacts_content_bytes() {
        let secret = b"Dean Pelton's secret shell setup";
        let edit = edit_config(secret, Shell::Bash).unwrap();
        let debug = format!("{edit:?}");

        assert!(!debug.contains("Dean Pelton"));
        assert!(debug.contains("content_bytes"));
        assert!(debug.contains("legacy_integration_count"));
    }

    #[test]
    fn newline_heavy_configs_are_scanned_in_one_pass() {
        let mut content = vec![b'\n'; 200_000];
        content.extend_from_slice(b"  argmax   init fish | source\n");

        let inspection = inspect_config(&content).unwrap();
        assert!(!inspection.has_marked_block());
        assert_eq!(
            inspection.legacy_integrations(),
            &[LegacyIntegration {
                shell: Shell::Fish,
                style: LegacyStyle::FishPipeSource,
                line: 200_001,
            }]
        );
    }
}
