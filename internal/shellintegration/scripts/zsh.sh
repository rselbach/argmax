# argmax shell integration
[[ -o aliases ]] && __argmax_zsh_restore_aliases=1 || __argmax_zsh_restore_aliases=0
\setopt no_aliases
\builtin eval 'if [[ -o interactive && -t 0 && -t 1 ]]; then
  if [[ -n ${ARGMAX_PRIVATE_SESSION-} ]]; then
    if [[ -z ${ARGMAX_SESSION_OWNER_PID-} ]]; then
      export ARGMAX_SESSION_OWNER_PID=$$
    elif [[ $ARGMAX_SESSION_OWNER_PID != "$$" ]]; then
      unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_CONTROL_FD \
        ARGMAX_ACTIVE_SHELL ARGMAX_SESSION_OWNER_PID
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
          $+functions[__argmax_control_apply] ||
          $+functions[__argmax_control_drain] ||
          $+parameters[__ARGMAX_ZSH_HOOKS] ||
          $+parameters[__ARGMAX_ZSH_CAPABILITY] ||
          $+parameters[__ARGMAX_ZSH_COMMAND_ACTIVE] ||
          $+parameters[__ARGMAX_ZSH_PROBE] ||
          $+parameters[__ARGMAX_ZSH_PROBE_NONCE] ||
          $+parameters[__ARGMAX_ZSH_PROBE_RESYNC_LAST_ID] ||
          $+parameters[__ARGMAX_ZSH_CONTROL_PENDING] ||
          $+parameters[__ARGMAX_ZSH_CONTROL_DISCARDING] ||
          $+parameters[__ARGMAX_ZSH_CONTROL_LAST_ID] ||
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
        print -rn -- capability:unavailable$'\''\0'\'' \
          2>/dev/null 1>&$ARGMAX_EVENT_FD || :
      fi
    else
      __ARGMAX_ZSH_HOOKS=argmax-owned-zsh-v1
      __ARGMAX_ZSH_COMMAND_ACTIVE=0
      __ARGMAX_ZSH_PROBE=$'\''\e[argmax-sync~'\''
      __ARGMAX_ZSH_PROBE_NONCE=0
      __ARGMAX_ZSH_PROBE_RESYNC_LAST_ID=0
      __ARGMAX_ZSH_CONTROL_PENDING=
      __ARGMAX_ZSH_CONTROL_DISCARDING=0
      __ARGMAX_ZSH_CONTROL_LAST_ID=0

      __argmax_emit() {
        : argmax-owned-zsh-v1
        emulate -L zsh
        local argmax_event=$1
        [[ ${ARGMAX_EVENT_FD-} == <-> ]] || return 0
        (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
        if (( ${#argmax_event} > 16417 )); then
          argmax_event=protocol-frame-oversized
        fi
        print -rn -- "$argmax_event"$'\''\0'\'' \
          2>/dev/null 1>&$ARGMAX_EVENT_FD || :
      }

      __argmax_preexec() {
        : argmax-owned-zsh-v1
        emulate -L zsh
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
        emulate -L zsh
        if (( ${__ARGMAX_ZSH_COMMAND_ACTIVE:-0} )); then
          __argmax_emit "command-stop:$argmax_status"
          __ARGMAX_ZSH_COMMAND_ACTIVE=0
        fi
        __argmax_emit "cwd:${PWD-}"
        __argmax_emit prompt-ready
        return $argmax_status
      }

      __argmax_control_apply() {
        : argmax-owned-zsh-v1
        emulate -L zsh
        local argmax_frame=$1
        local argmax_native_unit=$2
        local LC_ALL=C
        local argmax_rest
        local argmax_request
        local -i argmax_request_value
        local argmax_cursor
        local -i argmax_cursor_value
        local argmax_length
        local -i argmax_length_value
        local argmax_hex
        local argmax_pair
        local -i argmax_byte
        local -i argmax_byte_index=0
        local -i argmax_char_index=0
        local -i argmax_cursor_character=-1
        local -i argmax_width
        local -i argmax_offset
        local -i argmax_position
        local -i argmax_low
        local -i argmax_high
        local argmax_escapes=
        local argmax_decoded

        case $argmax_frame in
          argmax-control-v1:resync:*)
            argmax_request=${argmax_frame#argmax-control-v1:resync:}
            [[ $argmax_request == <-> && ${#argmax_request} -le 10 ]] ||
              return 0
            [[ $argmax_request[1] != 0 ]] || return 0
            argmax_request_value=$((10#$argmax_request))
            (( argmax_request_value >= 1 &&
               argmax_request_value <= 2147483647 )) || return 0
            (( argmax_request_value >
               __ARGMAX_ZSH_PROBE_RESYNC_LAST_ID )) || return 0
            argmax_probe_resync_request=$argmax_request_value
            argmax_probe_resync_ready=1
            __ARGMAX_ZSH_PROBE_RESYNC_LAST_ID=$argmax_request_value
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

        [[ $argmax_request == <-> && ${#argmax_request} -le 10 ]] ||
          return 0
        [[ ${#argmax_request} -eq 1 || $argmax_request[1] != 0 ]] ||
          return 0
        [[ $argmax_cursor == <-> && ${#argmax_cursor} -le 5 ]] || return 0
        [[ ${#argmax_cursor} -eq 1 || $argmax_cursor[1] != 0 ]] || return 0
        [[ $argmax_length == <-> && ${#argmax_length} -le 5 ]] || return 0
        [[ ${#argmax_length} -eq 1 || $argmax_length[1] != 0 ]] || return 0
        argmax_request_value=$((10#$argmax_request))
        argmax_cursor_value=$((10#$argmax_cursor))
        argmax_length_value=$((10#$argmax_length))
        (( argmax_request_value >= 1 &&
           argmax_request_value <= 2147483647 )) || return 0
        (( argmax_length_value <= 16384 )) || return 0
        (( argmax_cursor_value <= argmax_length_value )) || return 0
        (( ${#argmax_hex} == argmax_length_value * 2 )) || return 0
        [[ $argmax_hex != *[^0-9a-f]* ]] || return 0
        (( argmax_request_value == __ARGMAX_ZSH_PROBE_NONCE + 1 )) ||
          return 0
        (( argmax_request_value > __ARGMAX_ZSH_CONTROL_LAST_ID )) || return 0
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
        printf -v argmax_decoded '\''%b'\'' "$argmax_escapes" || return 0
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
        : argmax-owned-zsh-v1
        emulate -L zsh
        local argmax_native_unit=$1
        local LC_ALL=C
        local argmax_chunk
        local -i argmax_read_status
        local -i argmax_chunk_bytes
        local -i argmax_total_bytes=0
        local -i argmax_frames=0
        local argmax_frame
        local -a argmax_parts
        local -i argmax_part_index
        local argmax_part

        [[ ${ARGMAX_CONTROL_FD-} == <-> ]] || return 0
        (( 10#$ARGMAX_CONTROL_FD >= 3 )) || return 0
        while (( argmax_total_bytes < 65536 && argmax_frames < 4 )); do
          argmax_chunk=
          sysread -i $ARGMAX_CONTROL_FD -s 4096 -t 0 argmax_chunk \
            2>/dev/null
          argmax_read_status=$?
          (( argmax_read_status == 0 )) || break
          argmax_chunk_bytes=${#argmax_chunk}
          (( argmax_chunk_bytes > 0 )) || break
          ((argmax_total_bytes += argmax_chunk_bytes))
          argmax_parts=("${(@0)argmax_chunk}")
          for ((argmax_part_index = 1;
                argmax_part_index <= ${#argmax_parts};
                argmax_part_index++)); do
            argmax_part=$argmax_parts[$argmax_part_index]
            if (( __ARGMAX_ZSH_CONTROL_DISCARDING == 0 )); then
              __ARGMAX_ZSH_CONTROL_PENDING+=$argmax_part
              if (( ${#__ARGMAX_ZSH_CONTROL_PENDING} > 32817 )); then
                __ARGMAX_ZSH_CONTROL_PENDING=
                __ARGMAX_ZSH_CONTROL_DISCARDING=1
              fi
            fi
            (( argmax_part_index < ${#argmax_parts} )) || continue

            ((argmax_frames += 1))
            if (( __ARGMAX_ZSH_CONTROL_DISCARDING )); then
              __ARGMAX_ZSH_CONTROL_DISCARDING=0
              __ARGMAX_ZSH_CONTROL_PENDING=
              continue
            fi
            argmax_frame=$__ARGMAX_ZSH_CONTROL_PENDING
            __ARGMAX_ZSH_CONTROL_PENDING=
            if (( argmax_frames <= 4 )); then
              __argmax_control_apply "$argmax_frame" "$argmax_native_unit"
            fi
          done
        done
      }

      __argmax_sync() {
        local argmax_status=$?
        : argmax-owned-zsh-v1
        local argmax_unit=b
        if [[ -o multibyte ]]; then
          argmax_unit=c
        fi
        emulate -L zsh
        # The editor reports CURSOR and BUFFER in the caller'\''s own multibyte
        # mode, so the emulated default must not redefine those units.
        if [[ $argmax_unit == b ]]; then
          unsetopt multibyte
        fi
        local argmax_control_buffer=
        local -i argmax_control_cursor=0
        local -i argmax_control_request=0
        local -i argmax_control_ready=0
        local -i argmax_probe_resync_request=0
        local -i argmax_probe_resync_ready=0
        __argmax_control_drain "$argmax_unit"
        if (( argmax_control_ready )); then
          BUFFER=$argmax_control_buffer
          CURSOR=$argmax_control_cursor
          __ARGMAX_ZSH_CONTROL_LAST_ID=$argmax_control_request
        fi
        if (( argmax_probe_resync_ready )); then
          __argmax_emit \
            "probe-resync:$argmax_probe_resync_request:$__ARGMAX_ZSH_PROBE_NONCE"
          return $argmax_status
        fi
        if (( __ARGMAX_ZSH_PROBE_NONCE == 9223372036854775807 )); then
          __ARGMAX_ZSH_CAPABILITY=unavailable
          __argmax_emit capability:unavailable
          return $argmax_status
        fi
        ((__ARGMAX_ZSH_PROBE_NONCE += 1))
        if (( ${#BUFFER} > 16384 )); then
          __argmax_emit protocol-frame-oversized
          return $argmax_status
        fi
        __argmax_emit \
          "probe-buffer:$argmax_unit:$__ARGMAX_ZSH_PROBE_NONCE:$CURSOR:$BUFFER"
        return $argmax_status
      }

      autoload -Uz add-zsh-hook
      typeset -i __argmax_zsh_preexec_added=0
      typeset -i __argmax_zsh_precmd_added=0
      typeset -i __argmax_zsh_widget_added=0
      typeset -i __argmax_zsh_install_ok=1
      typeset -a __argmax_zsh_bound_maps=()
      typeset __argmax_zsh_binding

      if ! zmodload zsh/system 2>/dev/null; then
        __argmax_zsh_install_ok=0
      fi

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
          print -rn -- capability:unavailable$'\''\0'\'' \
            2>/dev/null 1>&$ARGMAX_EVENT_FD || :
        fi
        unfunction __argmax_emit __argmax_preexec __argmax_precmd \
          __argmax_sync __argmax_control_apply __argmax_control_drain \
          2>/dev/null || :
        unset __ARGMAX_ZSH_HOOKS __ARGMAX_ZSH_COMMAND_ACTIVE \
          __ARGMAX_ZSH_CAPABILITY __ARGMAX_ZSH_PROBE \
          __ARGMAX_ZSH_PROBE_NONCE __ARGMAX_ZSH_PROBE_RESYNC_LAST_ID \
          __ARGMAX_ZSH_CONTROL_PENDING __ARGMAX_ZSH_CONTROL_DISCARDING \
          __ARGMAX_ZSH_CONTROL_LAST_ID
      fi
      unset __argmax_zsh_preexec_added __argmax_zsh_precmd_added \
        __argmax_zsh_widget_added __argmax_zsh_install_ok \
        __argmax_zsh_bound_maps __argmax_zsh_binding __argmax_zsh_map
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -n ${__ARGMAX_ZSH_HOOKS-} ]] &&
      (( $+builtins[sysread] )) &&
      [[ ${__ARGMAX_ZSH_HOOKS-} == argmax-owned-zsh-v1 &&
         $(functions __argmax_emit 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_preexec 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_precmd 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_sync 2>/dev/null) == *argmax-owned-zsh-v1* &&
         $(functions __argmax_control_apply 2>/dev/null) == \
           *argmax-owned-zsh-v1* &&
         $(functions __argmax_control_drain 2>/dev/null) == \
           *argmax-owned-zsh-v1* &&
         ${__ARGMAX_ZSH_PROBE_RESYNC_LAST_ID-} == <-> &&
         ${#__ARGMAX_ZSH_PROBE_RESYNC_LAST_ID} -le 10 ]] &&
      (( 10#$__ARGMAX_ZSH_PROBE_RESYNC_LAST_ID <= 2147483647 )) &&
      [[ ${__ARGMAX_ZSH_CONTROL_DISCARDING-} == <0-1> &&
         ${__ARGMAX_ZSH_CONTROL_LAST_ID-} == <-> &&
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
      print -rn -- capability:unavailable$'\''\0'\'' \
        2>/dev/null 1>&$ARGMAX_EVENT_FD || :
    fi
  fi
fi
'
if (( __argmax_zsh_restore_aliases )); then
  \setopt aliases
fi
\builtin unset __argmax_zsh_restore_aliases
