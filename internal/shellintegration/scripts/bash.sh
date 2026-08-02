# argmax shell integration
if [[ $- == *i* && -t 0 && -t 1 ]]; then
  if [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ -z ${ARGMAX_SESSION_OWNER_PID-} ]]; then
      export ARGMAX_SESSION_OWNER_PID=$BASHPID
    elif [[ $ARGMAX_SESSION_OWNER_PID != "$BASHPID" ]]; then
      unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_CONTROL_FD \
        ARGMAX_ACTIVE_SHELL ARGMAX_SESSION_OWNER_PID
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
        builtin printf '%s\0' capability:unavailable \
          2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
      fi
    elif declare -F __argmax_emit >/dev/null ||
        declare -F __argmax_preexec >/dev/null ||
        declare -F __argmax_precmd >/dev/null ||
        declare -F __argmax_postprompt >/dev/null ||
        declare -F __argmax_sync >/dev/null ||
        declare -F __argmax_control_apply >/dev/null ||
        declare -F __argmax_control_drain >/dev/null ||
        declare -F __argmax_probe_is_unbound >/dev/null ||
        declare -F __argmax_install >/dev/null ||
        [[ -n ${__ARGMAX_BASH_HOOKS+x} ||
           -n ${__ARGMAX_BASH_CAPABILITY+x} ||
           -n ${__ARGMAX_BASH_COMMAND_ACTIVE+x} ||
           -n ${__ARGMAX_BASH_HISTORY_INDEX+x} ||
           -n ${__ARGMAX_BASH_MULTILINE+x} ||
           -n ${__ARGMAX_BASH_PROBE+x} ||
           -n ${__ARGMAX_BASH_PROBE_NONCE+x} ||
           -n ${__ARGMAX_BASH_PROBE_RESYNC_LAST_ID+x} ||
           -n ${__ARGMAX_BASH_CONTROL_PENDING+x} ||
           -n ${__ARGMAX_BASH_CONTROL_DISCARDING+x} ||
           -n ${__ARGMAX_BASH_CONTROL_LAST_ID+x} ||
           ${PS0-} == *'__argmax_preexec'* ||
           ${PS2-} == *'__ARGMAX_BASH_MULTILINE'* ||
           ${PROMPT_COMMAND[*]-} == *'__argmax_precmd'* ||
           ${PROMPT_COMMAND[*]-} == *'__argmax_postprompt'* ]]; then
      if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
          (( 10#$ARGMAX_EVENT_FD >= 3 )); then
        builtin printf '%s\0' capability:unavailable \
          2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
      fi
    else
      __argmax_install() {
        local argmax_install_ok=1
        local argmax_ps0_set=0
        local argmax_ps0_value=
        local argmax_ps0_declaration
        local argmax_ps2_set=0
        local argmax_ps2_value=
        local argmax_ps2_declaration
        local argmax_prompt_kind=unset
        local argmax_prompt_scalar=
        local argmax_prompt_value=
        local argmax_prompt_declaration
        local -a argmax_prompt_indices=()
        local -a argmax_prompt_values=()
        local argmax_emacs_attempted=0
        local argmax_vi_insert_attempted=0
        local argmax_vi_command_attempted=0
        local argmax_ps0_attempted=0
        local argmax_ps2_attempted=0
        local argmax_prompt_attempted=0
        local argmax_index
        local argmax_restore_index

        # The PS0/PS2 hooks rely on prompt-string expansion; without
        # promptvars their literal text would print before every command
        # and no lifecycle event would ever fire.
        builtin shopt -q promptvars || return 1

        if argmax_ps0_declaration=$(builtin declare -p PS0 2>/dev/null); then
          if [[ $argmax_ps0_declaration != 'declare -- PS0' &&
                $argmax_ps0_declaration != 'declare -- PS0='* ]]; then
            return 1
          fi
          argmax_ps0_set=1
          argmax_ps0_value=$PS0
        fi
        if argmax_ps2_declaration=$(builtin declare -p PS2 2>/dev/null); then
          if [[ $argmax_ps2_declaration != 'declare -- PS2' &&
                $argmax_ps2_declaration != 'declare -- PS2='* ]]; then
            return 1
          fi
          argmax_ps2_set=1
          argmax_ps2_value=$PS2
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
            ! __ARGMAX_BASH_HISTORY_INDEX=0 ||
            ! __ARGMAX_BASH_MULTILINE=0 ||
            ! __ARGMAX_BASH_PROBE=$'\e[argmax-sync~' ||
            ! __ARGMAX_BASH_PROBE_NONCE=0 ||
            ! __ARGMAX_BASH_PROBE_RESYNC_LAST_ID=0 ||
            ! __ARGMAX_BASH_CONTROL_PENDING= ||
            ! __ARGMAX_BASH_CONTROL_DISCARDING=0 ||
            ! __ARGMAX_BASH_CONTROL_LAST_ID=0; then
          argmax_install_ok=0
        fi

        __argmax_emit() {
          : argmax-owned-bash-v1
          local argmax_event=$1
          [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] || return 0
          (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
          if (( ${#argmax_event} > 16417 )); then
            argmax_event=protocol-frame-oversized
          fi
          builtin printf '%s\0' "$argmax_event" \
            2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
        }

        __argmax_preexec() {
          local argmax_status=$?
          : argmax-owned-bash-v1
          local argmax_history_index=${HISTCMD-}
          local argmax_history_output=
          local argmax_command=
          local argmax_exact=0

          if [[ -o history &&
                $argmax_history_index =~ ^[1-9][0-9]{0,17}$ &&
                $__ARGMAX_BASH_HISTORY_INDEX =~ ^(0|[1-9][0-9]{0,17})$ ]] &&
              (( 10#$argmax_history_index >
                 10#$__ARGMAX_BASH_HISTORY_INDEX )) &&
              { (( __ARGMAX_BASH_MULTILINE == 0 )) ||
                builtin shopt -q lithist; }; then
            argmax_history_output=$(
              builtin fc -ln -0 2>/dev/null
              argmax_fc_status=$?
              builtin printf '\001%s' "$argmax_fc_status"
            )
            if [[ $argmax_history_output == *$'\0010' ]]; then
              argmax_history_output=${argmax_history_output%$'\0010'}
              if [[ $argmax_history_output == $'\t '*$'\n' ]]; then
                argmax_command=${argmax_history_output#$'\t '}
                argmax_command=${argmax_command%$'\n'}
                if [[ -n $argmax_command &&
                      ${#argmax_command} -le 16384 ]]; then
                  argmax_exact=1
                fi
              fi
            fi
          fi

          if (( argmax_exact )); then
            __argmax_emit "command-start:$argmax_command"
          else
            __argmax_emit command-start-unknown
          fi
          return "$argmax_status"
        }

        __argmax_precmd() {
          local argmax_status=$?
          : argmax-owned-bash-v1
          if [[ -n ${__ARGMAX_BASH_COMMAND_ACTIVE+x} ]]; then
            __argmax_emit "command-stop:$argmax_status"
            builtin unset __ARGMAX_BASH_COMMAND_ACTIVE
          fi
          __argmax_emit "cwd:${PWD-}"
          __argmax_emit prompt-ready
          return "$argmax_status"
        }

        __argmax_postprompt() {
          local argmax_status=$?
          : argmax-owned-bash-v1
          __ARGMAX_BASH_HISTORY_INDEX=${HISTCMD-}
          __ARGMAX_BASH_MULTILINE=0
          return "$argmax_status"
        }

        __argmax_control_apply() {
          : argmax-owned-bash-v1
          local argmax_frame=$1
          local argmax_native_unit=$2
          local LC_ALL=C
          local argmax_rest
          local argmax_request
          local argmax_request_value
          local argmax_cursor
          local argmax_cursor_value
          local argmax_length
          local argmax_length_value
          local argmax_hex
          local argmax_pair
          local argmax_byte
          local argmax_byte_index=0
          local argmax_char_index=0
          local argmax_cursor_character=-1
          local argmax_width
          local argmax_offset
          local argmax_position
          local argmax_low
          local argmax_high
          local argmax_escapes=
          local argmax_decoded

          case $argmax_frame in
            argmax-control-v1:resync:*)
              argmax_request=${argmax_frame#argmax-control-v1:resync:}
              [[ $argmax_request =~ ^[1-9][0-9]{0,9}$ ]] || return 0
              argmax_request_value=$((10#$argmax_request))
              (( argmax_request_value <= 2147483647 )) || return 0
              (( argmax_request_value >
                 __ARGMAX_BASH_PROBE_RESYNC_LAST_ID )) || return 0
              argmax_probe_resync_request=$argmax_request_value
              argmax_probe_resync_ready=1
              __ARGMAX_BASH_PROBE_RESYNC_LAST_ID=$argmax_request_value
              return 0
              ;;
            argmax-control-v1:replace:*)
              argmax_rest=${argmax_frame#argmax-control-v1:replace:}
              ;;
            *) return 0 ;;
          esac
          [[ $argmax_rest == *:* ]] || return 0
          argmax_request=${argmax_rest%%:*}
          argmax_rest=${argmax_rest#*:}
          [[ $argmax_rest == *:* ]] || return 0
          argmax_cursor=${argmax_rest%%:*}
          argmax_rest=${argmax_rest#*:}
          [[ $argmax_rest == *:* ]] || return 0
          argmax_length=${argmax_rest%%:*}
          argmax_hex=${argmax_rest#*:}
          [[ $argmax_hex != *:* ]] || return 0

          [[ $argmax_request =~ ^[1-9][0-9]{0,9}$ ]] || return 0
          [[ $argmax_cursor =~ ^(0|[1-9][0-9]{0,4})$ ]] || return 0
          [[ $argmax_length =~ ^(0|[1-9][0-9]{0,4})$ ]] || return 0
          argmax_request_value=$((10#$argmax_request))
          argmax_cursor_value=$((10#$argmax_cursor))
          argmax_length_value=$((10#$argmax_length))
          (( argmax_request_value <= 2147483647 )) || return 0
          (( argmax_length_value <= 16384 )) || return 0
          (( argmax_cursor_value <= argmax_length_value )) || return 0
          (( ${#argmax_hex} == argmax_length_value * 2 )) || return 0
          [[ $argmax_hex != *[!0-9a-f]* ]] || return 0
          (( argmax_request_value == __ARGMAX_BASH_PROBE_NONCE + 1 )) ||
            return 0
          (( argmax_request_value > __ARGMAX_BASH_CONTROL_LAST_ID )) ||
            return 0
          (( argmax_control_ready == 0 )) || return 0

          while (( argmax_byte_index < argmax_length_value )); do
            if (( argmax_byte_index == argmax_cursor_value )); then
              argmax_cursor_character=$argmax_char_index
            fi
            argmax_position=$((argmax_byte_index * 2))
            argmax_pair=${argmax_hex:$argmax_position:2}
            argmax_byte=$((16#$argmax_pair))
            (( argmax_byte != 0 )) || return 0
            argmax_low=128
            argmax_high=191
            if (( argmax_byte <= 127 )); then
              argmax_width=1
            elif (( argmax_byte >= 194 && argmax_byte <= 223 )); then
              argmax_width=2
            elif (( argmax_byte >= 224 && argmax_byte <= 239 )); then
              argmax_width=3
              if (( argmax_byte == 224 )); then
                argmax_low=160
              elif (( argmax_byte == 237 )); then
                argmax_high=159
              fi
            elif (( argmax_byte >= 240 && argmax_byte <= 244 )); then
              argmax_width=4
              if (( argmax_byte == 240 )); then
                argmax_low=144
              elif (( argmax_byte == 244 )); then
                argmax_high=143
              fi
            else
              return 0
            fi
            (( argmax_byte_index + argmax_width <= argmax_length_value )) ||
              return 0
            argmax_escapes+="\\x$argmax_pair"
            for ((argmax_offset = 1;
                  argmax_offset < argmax_width;
                  argmax_offset++)); do
              argmax_position=$(((argmax_byte_index + argmax_offset) * 2))
              argmax_pair=${argmax_hex:$argmax_position:2}
              argmax_byte=$((16#$argmax_pair))
              if (( argmax_offset == 1 )); then
                (( argmax_byte >= argmax_low && argmax_byte <= argmax_high )) ||
                  return 0
              else
                (( argmax_byte >= 128 && argmax_byte <= 191 )) || return 0
              fi
              argmax_escapes+="\\x$argmax_pair"
            done
            ((argmax_byte_index += argmax_width))
            ((argmax_char_index += 1))
          done
          if (( argmax_byte_index == argmax_cursor_value )); then
            argmax_cursor_character=$argmax_char_index
          fi
          (( argmax_cursor_character >= 0 )) || return 0
          builtin printf -v argmax_decoded '%b' "$argmax_escapes" || return 0
          (( ${#argmax_decoded} == argmax_length_value )) || return 0

          argmax_control_buffer=$argmax_decoded
          if [[ $argmax_native_unit == b ]]; then
            argmax_control_cursor=$argmax_cursor_value
          else
            argmax_control_cursor=$argmax_cursor_character
          fi
          argmax_control_request=$argmax_request_value
          argmax_control_ready=1
        }

        __argmax_control_drain() {
          : argmax-owned-bash-v1
          local argmax_native_unit=$1
          local LC_ALL=C
          local argmax_chunk
          local argmax_read_status
          local argmax_chunk_bytes
          local argmax_total_bytes=0
          local argmax_frames=0
          local argmax_frame

          [[ ${ARGMAX_CONTROL_FD-} =~ ^[0-9]+$ ]] || return 0
          (( 10#$ARGMAX_CONTROL_FD >= 3 )) || return 0
          while (( argmax_total_bytes < 65536 && argmax_frames < 4 )); do
            argmax_chunk=
            IFS= builtin read -r -d '' -n 4096 \
              -u "$ARGMAX_CONTROL_FD" argmax_chunk 2>/dev/null
            argmax_read_status=$?
            argmax_chunk_bytes=${#argmax_chunk}
            ((argmax_total_bytes += argmax_chunk_bytes))
            if (( __ARGMAX_BASH_CONTROL_DISCARDING == 0 )); then
              __ARGMAX_BASH_CONTROL_PENDING+=$argmax_chunk
              if (( ${#__ARGMAX_BASH_CONTROL_PENDING} > 32817 )); then
                __ARGMAX_BASH_CONTROL_PENDING=
                __ARGMAX_BASH_CONTROL_DISCARDING=1
              fi
            fi
            if (( argmax_read_status != 0 )); then
              break
            fi
            if (( argmax_chunk_bytes == 4096 )); then
              continue
            fi

            ((argmax_frames += 1))
            if (( __ARGMAX_BASH_CONTROL_DISCARDING )); then
              __ARGMAX_BASH_CONTROL_DISCARDING=0
              __ARGMAX_BASH_CONTROL_PENDING=
              continue
            fi
            argmax_frame=$__ARGMAX_BASH_CONTROL_PENDING
            __ARGMAX_BASH_CONTROL_PENDING=
            __argmax_control_apply "$argmax_frame" "$argmax_native_unit"
          done
        }

        __argmax_sync() {
          local argmax_status=$?
          : argmax-owned-bash-v1
          local argmax_locale=${LC_ALL-}
          local argmax_unit=c
          local argmax_control_buffer=
          local argmax_control_cursor=0
          local argmax_control_request=0
          local argmax_control_ready=0
          local argmax_probe_resync_request=0
          local argmax_probe_resync_ready=0
          local argmax_buffer
          if [[ -z $argmax_locale ]]; then
            argmax_locale=${LC_CTYPE-}
          fi
          if [[ -z $argmax_locale ]]; then
            argmax_locale=${LANG-}
          fi
          if [[ $argmax_locale == C || $argmax_locale == POSIX ]]; then
            argmax_unit=b
          fi
          __argmax_control_drain "$argmax_unit"
          if (( argmax_control_ready )); then
            READLINE_LINE=$argmax_control_buffer
            READLINE_POINT=$argmax_control_cursor
            __ARGMAX_BASH_CONTROL_LAST_ID=$argmax_control_request
          fi
          if (( argmax_probe_resync_ready )); then
            __argmax_emit \
              "probe-resync:$argmax_probe_resync_request:$__ARGMAX_BASH_PROBE_NONCE"
            return "$argmax_status"
          fi
          argmax_buffer=$READLINE_LINE
          if (( __ARGMAX_BASH_PROBE_NONCE == 9223372036854775807 )); then
            __ARGMAX_BASH_CAPABILITY=unavailable
            __argmax_emit capability:unavailable
            return "$argmax_status"
          fi
          ((__ARGMAX_BASH_PROBE_NONCE += 1))
          if (( ${#argmax_buffer} > 16384 )); then
            __argmax_emit protocol-frame-oversized
            return "$argmax_status"
          fi
          __argmax_emit \
            "probe-buffer:$argmax_unit:$__ARGMAX_BASH_PROBE_NONCE:$READLINE_POINT:$argmax_buffer"
          return "$argmax_status"
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
          while IFS= builtin read -r argmax_binding; do
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
             ! builtin declare -F __argmax_postprompt >/dev/null ||
             ! builtin declare -F __argmax_sync >/dev/null ||
             ! builtin declare -F __argmax_control_apply >/dev/null ||
             ! builtin declare -F __argmax_control_drain >/dev/null ||
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
          argmax_ps2_attempted=1
          # Mark parser continuations without changing the visible prompt.
          # shellcheck disable=SC2016 # deliberately deferred to Bash
          PS2=${PS2-}'${__ARGMAX_BASH_HOOKS:$((__ARGMAX_BASH_MULTILINE=1)):0}' ||
            argmax_install_ok=0
        fi
        if (( argmax_install_ok )); then
          argmax_prompt_attempted=1
          if [[ $argmax_prompt_kind == array ]]; then
            PROMPT_COMMAND=(
              __argmax_precmd
              "${PROMPT_COMMAND[@]}"
              __argmax_postprompt
            ) ||
              argmax_install_ok=0
          else
            argmax_prompt_value=__argmax_precmd
            # shellcheck disable=SC2128 # scalar form
            argmax_prompt_value+=${PROMPT_COMMAND:+;$PROMPT_COMMAND}
            argmax_prompt_value+=$'\n__argmax_postprompt'
            # shellcheck disable=SC2178 # scalar form
            PROMPT_COMMAND=$argmax_prompt_value ||
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
        if (( argmax_ps2_attempted )); then
          if (( argmax_ps2_set )); then
            PS2=$argmax_ps2_value
          else
            builtin unset PS2 2>/dev/null || :
          fi
        fi
        if (( argmax_ps0_attempted )); then
          if (( argmax_ps0_set )); then
            PS0=$argmax_ps0_value
          else
            builtin unset PS0 2>/dev/null || :
          fi
        fi
        if (( argmax_vi_command_attempted )); then
          builtin bind -m vi-command -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        fi
        if (( argmax_vi_insert_attempted )); then
          builtin bind -m vi-insert -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        fi
        if (( argmax_emacs_attempted )); then
          builtin bind -m emacs-standard -r "$__ARGMAX_BASH_PROBE" \
            2>/dev/null || :
        fi
        builtin unset -f __argmax_emit __argmax_preexec __argmax_precmd \
          __argmax_postprompt __argmax_sync __argmax_control_apply \
          __argmax_control_drain __argmax_probe_is_unbound
        builtin unset __ARGMAX_BASH_HOOKS __ARGMAX_BASH_CAPABILITY \
          __ARGMAX_BASH_COMMAND_ACTIVE __ARGMAX_BASH_HISTORY_INDEX \
          __ARGMAX_BASH_MULTILINE __ARGMAX_BASH_PROBE \
          __ARGMAX_BASH_PROBE_NONCE __ARGMAX_BASH_PROBE_RESYNC_LAST_ID \
          __ARGMAX_BASH_CONTROL_PENDING __ARGMAX_BASH_CONTROL_DISCARDING \
          __ARGMAX_BASH_CONTROL_LAST_ID
        return 1
      }

      if ! __argmax_install; then
        if [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] &&
            (( 10#$ARGMAX_EVENT_FD >= 3 )); then
          builtin printf '%s\0' capability:unavailable \
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
          $(declare -f __argmax_postprompt 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_sync 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_control_apply 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_control_drain 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          $(declare -f __argmax_probe_is_unbound 2>/dev/null) == \
            *argmax-owned-bash-v1* &&
          ${__ARGMAX_BASH_COMMAND_ACTIVE-} == '' &&
          ${__ARGMAX_BASH_HISTORY_INDEX-} =~ ^[0-9]+$ &&
          ${__ARGMAX_BASH_MULTILINE-} == 0 &&
          ${__ARGMAX_BASH_PROBE_RESYNC_LAST_ID-} =~ \
            ^(0|[1-9][0-9]{0,9})$ ]] &&
      (( 10#$__ARGMAX_BASH_PROBE_RESYNC_LAST_ID <= 2147483647 )) &&
      [[ ${__ARGMAX_BASH_CONTROL_DISCARDING-} =~ ^[01]$ &&
          ${__ARGMAX_BASH_CONTROL_LAST_ID-} =~ ^[0-9]+$ &&
          ${PS0-} == *'__argmax_preexec'* &&
          ${PS2-} == *'__ARGMAX_BASH_MULTILINE'* &&
          ${PROMPT_COMMAND[*]-} == *'__argmax_precmd'* &&
          ${PROMPT_COMMAND[*]-} == *'__argmax_postprompt'* &&
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
      builtin printf '%s\0' capability:unavailable \
        2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
    fi
  fi
fi
