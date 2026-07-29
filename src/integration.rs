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

const BASH_INIT: &str = r#"# argmax shell integration
if [[ $- == *i* && -t 0 && -t 1 ]]; then
  if [[ -n ${TMUX-} && -n ${ARGMAX_PRIVATE_SESSION-} ]] &&
      command ps -o comm= -p "$PPID" 2>/dev/null | command grep -q 'tmux'; then
    unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL
  fi

  if [[ -z ${ARGMAX_PRIVATE_SESSION-} &&
        -z ${BASH_EXECUTION_STRING-} && $# -eq 0 ]]; then
    if command -v argmax >/dev/null 2>&1; then
      export ARGMAX_ACTIVE_SHELL=bash
      exec argmax --shell bash
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -z ${__ARGMAX_BASH_HOOKS-} ]]; then
    __ARGMAX_BASH_HOOKS=1

    __argmax_emit() {
      local argmax_event=$1
      [[ ${ARGMAX_EVENT_FD-} =~ ^[0-9]+$ ]] || return 0
      (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
      printf '%s\0' "$argmax_event" 2>/dev/null 1>&"$ARGMAX_EVENT_FD" || :
    }

    __argmax_preexec() {
      __argmax_emit command-start
    }

    __argmax_precmd() {
      local argmax_status=$?
      if [[ ${__ARGMAX_BASH_READY-0} == 1 ]]; then
        __argmax_emit "command-stop:$argmax_status"
      else
        __ARGMAX_BASH_READY=1
      fi
      __argmax_emit 'buffer:'
      return "$argmax_status"
    }

    __ARGMAX_BASH_PS0=${PS0-}
    # shellcheck disable=SC2016 # expansion is deliberately deferred to Bash
    PS0='${__ARGMAX_BASH_PS0}$(__argmax_preexec)'
    if declare -p PROMPT_COMMAND 2>/dev/null |
        command grep -q '^declare -a'; then
      PROMPT_COMMAND=(__argmax_precmd "${PROMPT_COMMAND[@]}")
    else
      # shellcheck disable=SC2128,SC2178 # this branch is the scalar form
      PROMPT_COMMAND="__argmax_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    fi
  fi
fi
"#;

const ZSH_INIT: &str = r#"# argmax shell integration
if [[ -o interactive && -t 0 && -t 1 ]]; then
  if [[ -n ${TMUX-} && -n ${ARGMAX_PRIVATE_SESSION-} ]] &&
      command ps -o comm= -p "$PPID" 2>/dev/null | command grep -q 'tmux'; then
    unset ARGMAX_PRIVATE_SESSION ARGMAX_EVENT_FD ARGMAX_ACTIVE_SHELL
  fi

  if [[ -z ${ARGMAX_PRIVATE_SESSION-} &&
        -z ${ZSH_EXECUTION_STRING-} && $# -eq 0 ]]; then
    if (( $+commands[argmax] )); then
      export ARGMAX_ACTIVE_SHELL=zsh
      exec argmax --shell zsh
    fi
  elif [[ -n ${ARGMAX_PRIVATE_SESSION-} &&
          -z ${__ARGMAX_ZSH_HOOKS-} ]]; then
    __ARGMAX_ZSH_HOOKS=1

    __argmax_emit() {
      local argmax_event=$1
      [[ ${ARGMAX_EVENT_FD-} == <-> ]] || return 0
      (( 10#$ARGMAX_EVENT_FD >= 3 )) || return 0
      print -rn -- "$argmax_event"$'\0' 2>/dev/null 1>&$ARGMAX_EVENT_FD || :
    }

    __argmax_preexec() {
      __ARGMAX_ZSH_COMMAND_ACTIVE=1
      __argmax_emit "buffer:$1"
      __argmax_emit command-start
    }

    __argmax_precmd() {
      local argmax_status=$?
      if (( ${__ARGMAX_ZSH_COMMAND_ACTIVE:-0} )); then
        __argmax_emit "command-stop:$argmax_status"
        __ARGMAX_ZSH_COMMAND_ACTIVE=0
      fi
      __argmax_emit 'buffer:'
      return $argmax_status
    }

    __argmax_buffer() {
      __argmax_emit "buffer:$BUFFER"
    }

    autoload -Uz add-zsh-hook add-zle-hook-widget
    add-zsh-hook -d preexec __argmax_preexec 2>/dev/null || :
    add-zsh-hook -d precmd __argmax_precmd 2>/dev/null || :
    add-zle-hook-widget -d line-pre-redraw __argmax_buffer 2>/dev/null || :
    add-zsh-hook preexec __argmax_preexec
    add-zsh-hook precmd __argmax_precmd
    add-zle-hook-widget line-pre-redraw __argmax_buffer
  fi
fi
"#;

const FISH_INIT: &str = r#"# argmax shell integration
if status is-interactive; and test -t 0; and test -t 1
  if set -q TMUX ARGMAX_PRIVATE_SESSION
    set -l argmax_parent (command ps -o comm= -p $PPID 2>/dev/null | string trim)
    if string match -q '*tmux*' -- $argmax_parent
      set -e ARGMAX_PRIVATE_SESSION
      set -e ARGMAX_EVENT_FD
      set -e ARGMAX_ACTIVE_SHELL
    end
  end

  if not set -q ARGMAX_PRIVATE_SESSION
    if command -q argmax
      set -gx ARGMAX_ACTIVE_SHELL fish
      exec argmax --shell fish
    end
  else
    functions -e __argmax_emit __argmax_preexec __argmax_postexec \
      __argmax_posterror 2>/dev/null

    function __argmax_emit
      set -q ARGMAX_EVENT_FD; or return 0
      string match -qr '^[0-9]+$' -- $ARGMAX_EVENT_FD; or return 0
      test $ARGMAX_EVENT_FD -ge 3; or return 0
      printf '%s\0' "$argv[1]" 2>/dev/null 1>&$ARGMAX_EVENT_FD; or true
    end

    function __argmax_preexec --on-event fish_preexec
      __argmax_emit "buffer:$argv"
      __argmax_emit command-start
    end

    function __argmax_postexec --on-event fish_postexec
      set -l argmax_status $status
      __argmax_emit "command-stop:$argmax_status"
      __argmax_emit 'buffer:'
      return $argmax_status
    end

    function __argmax_posterror --on-event fish_posterror
      set -l argmax_status $status
      __argmax_emit "command-stop:$argmax_status"
      __argmax_emit 'buffer:'
      return $argmax_status
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
/// An empty `ZDOTDIR` or `XDG_CONFIG_HOME` is treated as unset.
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
        Shell::Zsh => zdotdir.filter(nonempty).unwrap_or(home).join(".zshrc"),
        Shell::Fish => xdg_config_home
            .filter(nonempty)
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigEdit {
    content: Vec<u8>,
    outcome: EditOutcome,
    legacy_integrations: Vec<LegacyIntegration>,
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
    let marked_range = find_marked_range(content)?;
    let legacy_integrations = find_legacy_integrations(content, marked_range.as_ref());
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
    let marked_range = find_marked_range(content)?;
    let legacy_integrations = find_legacy_integrations(content, marked_range.as_ref());

    if let Some(range) = marked_range {
        let newline = preferred_newline(content);
        let block = render_block(shell, newline, false).into_bytes();
        if content[range.clone()] == block {
            return Ok(ConfigEdit {
                content: content.to_vec(),
                outcome: EditOutcome::Unchanged,
                legacy_integrations,
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
        });
    }

    let newline = preferred_newline(content);
    let mut edited = Vec::with_capacity(content.len() + 128);
    edited.extend_from_slice(content);
    if !content.is_empty() {
        if !content.ends_with(b"\n") {
            edited.extend_from_slice(newline);
        }
        if !ends_with_blank_line(content, newline) {
            edited.extend_from_slice(newline);
        }
    }
    edited.extend_from_slice(render_block(shell, newline, true).as_bytes());
    Ok(ConfigEdit {
        content: edited,
        outcome: EditOutcome::Appended,
        legacy_integrations,
    })
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

fn lines(content: &[u8]) -> Vec<Line<'_>> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut number = 1;
    while start < content.len() {
        let newline = content[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| start + offset);
        let raw_end = newline.unwrap_or(content.len());
        let content_end = if raw_end > start && content[raw_end - 1] == b'\r' {
            raw_end - 1
        } else {
            raw_end
        };
        result.push(Line {
            number,
            start,
            content: &content[start..content_end],
            content_end,
        });
        let Some(newline) = newline else {
            break;
        };
        start = newline + 1;
        number += 1;
    }
    result
}

fn marker(line: &[u8]) -> Option<Marker> {
    match line {
        bytes if bytes == BEGIN_MARKER.as_bytes() => Some(Marker::Begin),
        bytes if bytes == END_MARKER.as_bytes() => Some(Marker::End),
        _ => None,
    }
}

fn find_marked_range(content: &[u8]) -> Result<Option<Range<usize>>, ConfigEditError> {
    let mut open: Option<Line<'_>> = None;
    let mut found = None;
    for line in lines(content) {
        match marker(line.content) {
            Some(Marker::Begin) if open.is_some() => {
                return Err(ConfigEditError::NestedBeginMarker { line: line.number });
            }
            Some(Marker::Begin) if found.is_some() => {
                return Err(ConfigEditError::DuplicateBlock { line: line.number });
            }
            Some(Marker::Begin) => open = Some(line),
            Some(Marker::End) => {
                let Some(begin) = open.take() else {
                    return Err(ConfigEditError::UnexpectedEndMarker { line: line.number });
                };
                found = Some(begin.start..line.content_end);
            }
            None => {}
        }
    }
    if let Some(begin) = open {
        return Err(ConfigEditError::MissingEndMarker { line: begin.number });
    }
    Ok(found)
}

fn find_legacy_integrations(
    content: &[u8],
    marked_range: Option<&Range<usize>>,
) -> Vec<LegacyIntegration> {
    lines(content)
        .into_iter()
        .filter(|line| {
            marked_range
                .is_none_or(|range| line.content_end <= range.start || line.start >= range.end)
        })
        .filter_map(|line| {
            legacy_line(line.content).map(|(shell, style)| LegacyIntegration {
                shell,
                style,
                line: line.number,
            })
        })
        .collect()
}

fn legacy_line(line: &[u8]) -> Option<(Shell, LegacyStyle)> {
    let text = std::str::from_utf8(line).ok()?.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let direct = format!("argmax init {}", shell.as_str());
        let commanded = format!("command {direct}");
        if normalized == format!(r#"eval "$({direct})""#)
            || normalized == format!(r#"eval "$({commanded})""#)
        {
            return Some((shell, LegacyStyle::Eval));
        }
    }
    if normalized == "argmax init fish | source"
        || normalized == "command argmax init fish | source"
    {
        return Some((Shell::Fish, LegacyStyle::FishPipeSource));
    }
    None
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

fn ends_with_blank_line(content: &[u8], newline: &[u8]) -> bool {
    content.ends_with(&[newline, newline].concat())
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
            assert!(script.contains("command-start"));
            assert!(script.contains("command-stop:"));
            assert!(script.contains("buffer:"));
            assert!(!script.contains("set --"));
            assert!(!script.contains("shift"));
            assert!(!script.contains("$@"));
        }

        assert!(!init_script(Shell::Bash).contains("BASH_COMMAND"));
        assert!(init_script(Shell::Bash).contains("__ARGMAX_BASH_READY"));
        assert!(init_script(Shell::Zsh).contains("__ARGMAX_ZSH_COMMAND_ACTIVE"));
        assert!(init_script(Shell::Fish).contains("fish_posterror"));
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
            suggest_config_target(Shell::Zsh, home, Some(Path::new("")), None).path(),
            Path::new("/Users/troy/.zshrc")
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
}
