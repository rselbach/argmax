# argmax shell integration
if status is-interactive; and test -t 0; and test -t 1
  if set -q ARGMAX_PRIVATE_SESSION
    if not set -q ARGMAX_SESSION_OWNER_PID
      set -gx ARGMAX_SESSION_OWNER_PID $fish_pid
    else if test "$ARGMAX_SESSION_OWNER_PID" != "$fish_pid"
      set -e ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_CONTROL_FD \
        ARGMAX_ACTIVE_SHELL ARGMAX_SESSION_OWNER_PID
    end
  end

  if not set -q ARGMAX_PRIVATE_SESSION
    if command -q argmax
      set -gx ARGMAX_ACTIVE_SHELL fish
      exec argmax --shell fish
    end
  else
    set -l argmax_function_collision 0
    set -l argmax_functions __argmax_emit __argmax_sync \
      __argmax_control_apply __argmax_control_drain __argmax_preexec \
      __argmax_postexec __argmax_posterror __argmax_prompt
    if set -q __ARGMAX_FISH_INSTALLED
      if test "$__ARGMAX_FISH_INSTALLED" != argmax-owned-fish-v1; or \
          not set -q __ARGMAX_FISH_CAPABILITY; or \
          test "$__ARGMAX_FISH_CAPABILITY" != probe; or \
          not set -q __ARGMAX_FISH_COMMAND_ACTIVE; or \
          not set -q __ARGMAX_FISH_PROBE_NONCE; or not \
          string match -qr '^[0-9]+$' -- $__ARGMAX_FISH_PROBE_NONCE; or \
          not set -q __ARGMAX_FISH_PROBE_RESYNC_PENDING; or \
          test -n "$__ARGMAX_FISH_PROBE_RESYNC_PENDING"; or \
          not set -q __ARGMAX_FISH_PROBE_RESYNC_LAST_ID; or not \
          string match -qr '^(0|[1-9][0-9]{0,9})$' -- \
            $__ARGMAX_FISH_PROBE_RESYNC_LAST_ID; or \
          test $__ARGMAX_FISH_PROBE_RESYNC_LAST_ID -gt 2147483647; or \
          not set -q __ARGMAX_FISH_CONTROL_PENDING; or \
          not set -q __ARGMAX_FISH_CONTROL_DISCARDING; or not \
          string match -qr '^[01]$' -- $__ARGMAX_FISH_CONTROL_DISCARDING; or \
          not set -q __ARGMAX_FISH_CONTROL_LAST_ID; or not \
          string match -qr '^[0-9]+$' -- $__ARGMAX_FISH_CONTROL_LAST_ID
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
          set -q __ARGMAX_FISH_CAPABILITY; or \
          set -q __ARGMAX_FISH_PROBE_RESYNC_PENDING; or \
          set -q __ARGMAX_FISH_PROBE_RESYNC_LAST_ID; or \
          set -q __ARGMAX_FISH_CONTROL_PENDING; or \
          set -q __ARGMAX_FISH_CONTROL_DISCARDING; or \
          set -q __ARGMAX_FISH_CONTROL_LAST_ID
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
      if test (string length -- "$argmax_event") -gt 16417
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
      __argmax_emit "cwd:$PWD"
      __argmax_emit prompt-ready
    end

    function __argmax_control_apply
      set -l argmax_status $status
      set -l __argmax_fish_owner argmax-owned-fish-v1
      set -l argmax_frame "$argv[1]"
      if string match -qr \
          '^argmax-control-v1:resync:[1-9][0-9]{0,9}$' -- \
          "$argmax_frame"
        set -l argmax_request \
          (string replace 'argmax-control-v1:resync:' '' -- "$argmax_frame")
        test $argmax_request -le 2147483647; or return $argmax_status
        test $argmax_request -gt $__ARGMAX_FISH_PROBE_RESYNC_LAST_ID; or \
          return $argmax_status
        set -g __ARGMAX_FISH_PROBE_RESYNC_PENDING $argmax_request
        set -g __ARGMAX_FISH_PROBE_RESYNC_LAST_ID $argmax_request
        return $argmax_status
      end
      # The sentinel preserves a valid empty final hex field through command
      # substitution without giving control data any evaluation semantics.
      set -l argmax_fields (string split ':' -- "$argmax_frame"x)
      test (count $argmax_fields) -eq 6; or return $argmax_status
      test "$argmax_fields[1]" = argmax-control-v1; or \
        return $argmax_status
      test "$argmax_fields[2]" = replace; or return $argmax_status
      set -l argmax_request "$argmax_fields[3]"
      set -l argmax_cursor "$argmax_fields[4]"
      set -l argmax_length "$argmax_fields[5]"
      set -l argmax_hex ''
      set -l argmax_hex_with_sentinel "$argmax_fields[6]"
      set -l argmax_hex_with_sentinel_length \
        (string length -- "$argmax_hex_with_sentinel")
      test $argmax_hex_with_sentinel_length -ge 1; or return $argmax_status
      if test $argmax_hex_with_sentinel_length -gt 1
        set argmax_hex (string sub -s 1 \
          -l (math "$argmax_hex_with_sentinel_length - 1") -- \
          "$argmax_hex_with_sentinel")
      end

      string match -qr '^[1-9][0-9]{0,9}$' -- "$argmax_request"; or \
        return $argmax_status
      string match -qr '^(0|[1-9][0-9]{0,4})$' -- "$argmax_cursor"; or \
        return $argmax_status
      string match -qr '^(0|[1-9][0-9]{0,4})$' -- "$argmax_length"; or \
        return $argmax_status
      test $argmax_request -le 2147483647; or return $argmax_status
      test $argmax_length -le 16384; or return $argmax_status
      test $argmax_cursor -le $argmax_length; or return $argmax_status
      test (string length -- "$argmax_hex") -eq \
        (math "$argmax_length * 2"); or return $argmax_status
      string match -qr '^[0-9a-f]*$' -- "$argmax_hex"; or \
        return $argmax_status
      set -l argmax_expected_request \
        (math "$__ARGMAX_FISH_PROBE_NONCE + 1")
      test $argmax_request -eq $argmax_expected_request; or \
        return $argmax_status
      test $argmax_request -gt $__ARGMAX_FISH_CONTROL_LAST_ID; or \
        return $argmax_status

      set -l argmax_byte_index 0
      set -l argmax_char_index 0
      set -l argmax_cursor_character -1
      set -l argmax_escapes ''
      while test $argmax_byte_index -lt $argmax_length
        if test $argmax_byte_index -eq $argmax_cursor
          set argmax_cursor_character $argmax_char_index
        end
        set -l argmax_position (math "$argmax_byte_index * 2 + 1")
        set -l argmax_pair \
          (string sub -s $argmax_position -l 2 -- "$argmax_hex")
        set -l argmax_byte (math "0x$argmax_pair")
        test $argmax_byte -ne 0; or return $argmax_status
        set -l argmax_width
        set -l argmax_low 128
        set -l argmax_high 191
        if test $argmax_byte -le 127
          set argmax_width 1
        else if test $argmax_byte -ge 194; and test $argmax_byte -le 223
          set argmax_width 2
        else if test $argmax_byte -ge 224; and test $argmax_byte -le 239
          set argmax_width 3
          if test $argmax_byte -eq 224
            set argmax_low 160
          else if test $argmax_byte -eq 237
            set argmax_high 159
          end
        else if test $argmax_byte -ge 240; and test $argmax_byte -le 244
          set argmax_width 4
          if test $argmax_byte -eq 240
            set argmax_low 144
          else if test $argmax_byte -eq 244
            set argmax_high 143
          end
        else
          return $argmax_status
        end
        test (math "$argmax_byte_index + $argmax_width") \
          -le $argmax_length; or return $argmax_status
        set argmax_escapes "$argmax_escapes\\x$argmax_pair"
        set -l argmax_offset 1
        while test $argmax_offset -lt $argmax_width
          set argmax_position \
            (math "($argmax_byte_index + $argmax_offset) * 2 + 1")
          set argmax_pair \
            (string sub -s $argmax_position -l 2 -- "$argmax_hex")
          set argmax_byte (math "0x$argmax_pair")
          if test $argmax_offset -eq 1
            test $argmax_byte -ge $argmax_low; and \
              test $argmax_byte -le $argmax_high; or \
              return $argmax_status
          else
            test $argmax_byte -ge 128; and test $argmax_byte -le 191; or \
              return $argmax_status
          end
          set argmax_escapes "$argmax_escapes\\x$argmax_pair"
          set argmax_offset (math "$argmax_offset + 1")
        end
        set argmax_byte_index \
          (math "$argmax_byte_index + $argmax_width")
        set argmax_char_index (math "$argmax_char_index + 1")
      end
      if test $argmax_byte_index -eq $argmax_cursor
        set argmax_cursor_character $argmax_char_index
      end
      test $argmax_cursor_character -ge 0; or return $argmax_status

      set -l argmax_decoded ''
      if test $argmax_length -gt 0
        set argmax_decoded \
          (printf '%b' "$argmax_escapes" | string collect -N)
        test (count $argmax_decoded) -eq 1; or return $argmax_status
      end
      commandline -r -- "$argmax_decoded"; or return $argmax_status
      commandline -C $argmax_cursor_character; or return $argmax_status
      set -g __ARGMAX_FISH_CONTROL_LAST_ID $argmax_request
      return $argmax_status
    end

    function __argmax_control_drain
      set -l argmax_status $status
      set -l __argmax_fish_owner argmax-owned-fish-v1
      set -q ARGMAX_CONTROL_FD; or return $argmax_status
      string match -qr '^[0-9]+$' -- $ARGMAX_CONTROL_FD; or \
        return $argmax_status
      test $ARGMAX_CONTROL_FD -ge 3; or return $argmax_status
      set -l fish_read_limit 65536
      set -l argmax_total_bytes 0
      set -l argmax_frames 0
      while test $argmax_total_bytes -lt 65536; and \
          test $argmax_frames -lt 4
        # fish 3.x waits before reading even when this descriptor is
        # nonblocking. dd performs the nonblocking read; split0 then parses
        # the bounded pipe after dd exits without losing NUL frame boundaries.
        set -l argmax_parts (
          begin
            printf 'argmax-read-start\0'
            command dd bs=4096 count=1 \
              <&$ARGMAX_CONTROL_FD 2>/dev/null
            printf '\0argmax-read-end\0'
          end | string split0
        )
        test (count $argmax_parts) -ge 3; or break
        test "$argmax_parts[1]" = argmax-read-start; or break
        test "$argmax_parts[-1]" = argmax-read-end; or break
        set -e argmax_parts[1]
        set -e argmax_parts[-1]

        set -l argmax_chunk_bytes \
          (math (count $argmax_parts) - 1)
        for argmax_part in $argmax_parts
          set argmax_chunk_bytes (math \
            "$argmax_chunk_bytes + "(string length -- "$argmax_part"))
        end
        test $argmax_chunk_bytes -gt 0; or break
        set argmax_total_bytes \
          (math "$argmax_total_bytes + $argmax_chunk_bytes")

        set -l argmax_partial ''
        if test -n "$argmax_parts[-1]"
          set argmax_partial "$argmax_parts[-1]"
        end
        set -e argmax_parts[-1]

        for argmax_part in $argmax_parts
          test $argmax_frames -lt 4; or break
          if test $__ARGMAX_FISH_CONTROL_DISCARDING -eq 0
            set -g __ARGMAX_FISH_CONTROL_PENDING \
              "$__ARGMAX_FISH_CONTROL_PENDING$argmax_part"
            if test (string length -- "$__ARGMAX_FISH_CONTROL_PENDING") \
                -gt 32817
              set -g __ARGMAX_FISH_CONTROL_PENDING ''
              set -g __ARGMAX_FISH_CONTROL_DISCARDING 1
            end
          end
          set argmax_frames (math "$argmax_frames + 1")
          if test $__ARGMAX_FISH_CONTROL_DISCARDING -eq 1
            set -g __ARGMAX_FISH_CONTROL_DISCARDING 0
            set -g __ARGMAX_FISH_CONTROL_PENDING ''
            continue
          end
          set -l argmax_frame "$__ARGMAX_FISH_CONTROL_PENDING"
          set -g __ARGMAX_FISH_CONTROL_PENDING ''
          __argmax_control_apply "$argmax_frame"
        end

        if test -n "$argmax_partial"; and \
            test $__ARGMAX_FISH_CONTROL_DISCARDING -eq 0
          set -g __ARGMAX_FISH_CONTROL_PENDING \
            "$__ARGMAX_FISH_CONTROL_PENDING$argmax_partial"
          if test (string length -- "$__ARGMAX_FISH_CONTROL_PENDING") \
              -gt 32817
            set -g __ARGMAX_FISH_CONTROL_PENDING ''
            set -g __ARGMAX_FISH_CONTROL_DISCARDING 1
          end
        end
      end
      return $argmax_status
    end

    function __argmax_sync
      set -l argmax_status $status
      set -l __argmax_fish_owner argmax-owned-fish-v1
      __argmax_control_drain
      if test -n "$__ARGMAX_FISH_PROBE_RESYNC_PENDING"
        __argmax_emit \
          "probe-resync:$__ARGMAX_FISH_PROBE_RESYNC_PENDING:$__ARGMAX_FISH_PROBE_NONCE"
        set -g __ARGMAX_FISH_PROBE_RESYNC_PENDING ''
        return $argmax_status
      end
      # commandline prints one newline; the decoder removes only that terminator.
      set -l argmax_buffer (commandline -b | string collect -N)
      set -l argmax_cursor (commandline -C)
      if test $__ARGMAX_FISH_PROBE_NONCE -ge 2147483647
        set -g __ARGMAX_FISH_CAPABILITY unavailable
        __argmax_emit capability:unavailable
        return $argmax_status
      end
      set -g __ARGMAX_FISH_PROBE_NONCE \
        (math $__ARGMAX_FISH_PROBE_NONCE + 1)
      if test (string length -- "$argmax_buffer") -gt 16385
        __argmax_emit protocol-frame-oversized
        return $argmax_status
      end
      __argmax_emit \
        "probe-buffer:f:$__ARGMAX_FISH_PROBE_NONCE:$argmax_cursor:$argmax_buffer"
      return $argmax_status
    end

    set -g __ARGMAX_FISH_COMMAND_ACTIVE 0
    if not set -q __ARGMAX_FISH_PROBE_NONCE
      set -g __ARGMAX_FISH_PROBE_NONCE 0
    end
    if not set -q __ARGMAX_FISH_PROBE_RESYNC_PENDING
      set -g __ARGMAX_FISH_PROBE_RESYNC_PENDING ''
    end
    if not set -q __ARGMAX_FISH_PROBE_RESYNC_LAST_ID
      set -g __ARGMAX_FISH_PROBE_RESYNC_LAST_ID 0
    end
    if not set -q __ARGMAX_FISH_CONTROL_PENDING
      set -g __ARGMAX_FISH_CONTROL_PENDING ''
    end
    if not set -q __ARGMAX_FISH_CONTROL_DISCARDING
      set -g __ARGMAX_FISH_CONTROL_DISCARDING 0
    end
    if not set -q __ARGMAX_FISH_CONTROL_LAST_ID
      set -g __ARGMAX_FISH_CONTROL_LAST_ID 0
    end
    set -l argmax_probe \x1e
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
        __ARGMAX_FISH_COMMAND_ACTIVE __ARGMAX_FISH_PROBE_NONCE \
        __ARGMAX_FISH_PROBE_RESYNC_PENDING \
        __ARGMAX_FISH_PROBE_RESYNC_LAST_ID __ARGMAX_FISH_CONTROL_PENDING \
        __ARGMAX_FISH_CONTROL_DISCARDING __ARGMAX_FISH_CONTROL_LAST_ID
    end
    end
  end
end
