//! Side-effect-free parsing for the documented command-line contract.

use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, error::ErrorKind};

use crate::config::Shell;

/// One validated top-level invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invocation {
    /// Start an interactive wrapped shell.
    Run {
        /// Optional shell override for this session.
        shell: Option<Shell>,
        /// Whether diagnostic logging is enabled for this session.
        debug: bool,
    },
    /// Print sourceable integration code for one shell.
    Init {
        /// Shell whose integration code is requested.
        shell: Shell,
    },
    /// Install one idempotent shell integration block.
    Setup {
        /// Explicit shell, or none when runtime detection is requested.
        shell: Option<Shell>,
    },
    /// Create the commented default configuration.
    ConfigInit,
    /// Print the fully resolved, redacted configuration.
    ConfigShow,
    /// Reload the active wrapper session.
    Reload,
    /// Print the running semantic version.
    Version,
    /// Check for and apply an update.
    Update,
    /// Locate or remove private crash reports.
    CrashLog {
        /// Remove every argmax-owned crash report when true.
        clear: bool,
    },
    /// Remove shell integration and authorized local data.
    Uninstall,
}

impl Invocation {
    /// Parses an explicit argument sequence without reading the environment or
    /// performing any requested operation.
    ///
    /// The first item is treated as the binary name, matching
    /// [`clap::Parser::try_parse_from`]. Wrapper-only `--shell` and `--debug`
    /// flags are rejected when a subcommand is present.
    ///
    /// # Errors
    ///
    /// Returns a structured Clap error for malformed syntax, unsupported shell
    /// names, missing nested subcommands, or wrapper flags used with a
    /// non-interactive subcommand.
    pub fn try_parse_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let parsed = CliArguments::try_parse_from(arguments)?;
        if parsed.command.is_some() && (parsed.shell.is_some() || parsed.debug) {
            return Err(CliArguments::command().error(
                ErrorKind::ArgumentConflict,
                "--shell and --debug apply only when starting an interactive session",
            ));
        }

        Ok(match parsed.command {
            None => Self::Run {
                shell: parsed.shell.map(Into::into),
                debug: parsed.debug,
            },
            Some(CliCommand::Init { shell }) => Self::Init {
                shell: shell.into(),
            },
            Some(CliCommand::Setup { shell }) => Self::Setup {
                shell: shell.map(Into::into),
            },
            Some(CliCommand::Config { command }) => match command {
                ConfigCommand::Init => Self::ConfigInit,
                ConfigCommand::Show => Self::ConfigShow,
            },
            Some(CliCommand::Reload) => Self::Reload,
            Some(CliCommand::Version) => Self::Version,
            Some(CliCommand::Update) => Self::Update,
            Some(CliCommand::CrashLog { clear }) => Self::CrashLog { clear },
            Some(CliCommand::Uninstall) => Self::Uninstall,
        })
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "argmax",
    bin_name = "argmax",
    about = "Local terminal-native command assistant",
    disable_help_subcommand = true,
    disable_version_flag = true
)]
struct CliArguments {
    /// Override the shell for this interactive session.
    #[arg(long, value_enum)]
    shell: Option<ShellArgument>,

    /// Enable private diagnostic logging for this interactive session.
    #[arg(long)]
    debug: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Print sourceable integration code and nothing else on standard output.
    Init {
        /// Shell whose integration should be generated.
        #[arg(value_enum)]
        shell: ShellArgument,
    },
    /// Add one marked integration block to a shell configuration.
    Setup {
        /// Shell to configure; omitted to request supported-shell detection.
        #[arg(value_enum)]
        shell: Option<ShellArgument>,
    },
    /// Manage the user configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Reload the active argmax wrapper session.
    Reload,
    /// Print the running version.
    Version,
    /// Check for and apply an update.
    Update,
    /// Locate or clear private crash reports.
    CrashLog {
        /// Remove argmax-owned crash reports.
        #[arg(long)]
        clear: bool,
    },
    /// Remove shell integration and authorized local data.
    Uninstall,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Create a commented default configuration when none exists.
    Init,
    /// Print the fully resolved, secret-redacted configuration.
    Show,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ShellArgument {
    Bash,
    Zsh,
    Fish,
}

impl From<ShellArgument> for Shell {
    fn from(shell: ShellArgument) -> Self {
        match shell {
            ShellArgument::Bash => Self::Bash,
            ShellArgument::Zsh => Self::Zsh,
            ShellArgument::Fish => Self::Fish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Invocation {
        Invocation::try_parse_from(arguments).unwrap()
    }

    #[test]
    fn empty_and_wrapper_flags_resolve_only_to_interactive_runs() {
        assert_eq!(
            parse(&["argmax"]),
            Invocation::Run {
                shell: None,
                debug: false,
            }
        );
        assert_eq!(
            parse(&["argmax", "--shell", "fish", "--debug"]),
            Invocation::Run {
                shell: Some(Shell::Fish),
                debug: true,
            }
        );

        let error = Invocation::try_parse_from(["argmax", "--debug", "version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        let error = Invocation::try_parse_from(["argmax", "--shell", "bash", "config", "show"])
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn every_documented_subcommand_has_one_unambiguous_shape() {
        for (arguments, want) in [
            (
                vec!["argmax", "init", "bash"],
                Invocation::Init { shell: Shell::Bash },
            ),
            (vec!["argmax", "setup"], Invocation::Setup { shell: None }),
            (
                vec!["argmax", "setup", "zsh"],
                Invocation::Setup {
                    shell: Some(Shell::Zsh),
                },
            ),
            (vec!["argmax", "config", "init"], Invocation::ConfigInit),
            (vec!["argmax", "config", "show"], Invocation::ConfigShow),
            (vec!["argmax", "reload"], Invocation::Reload),
            (vec!["argmax", "version"], Invocation::Version),
            (vec!["argmax", "update"], Invocation::Update),
            (
                vec!["argmax", "crash-log"],
                Invocation::CrashLog { clear: false },
            ),
            (
                vec!["argmax", "crash-log", "--clear"],
                Invocation::CrashLog { clear: true },
            ),
            (vec!["argmax", "uninstall"], Invocation::Uninstall),
        ] {
            assert_eq!(parse(&arguments), want);
        }
    }

    #[test]
    fn shell_names_are_closed_and_case_sensitive() {
        for shell in ["bash", "zsh", "fish"] {
            assert!(Invocation::try_parse_from(["argmax", "init", shell]).is_ok());
        }
        for shell in ["sh", "Bash", "powershell", ""] {
            let error = Invocation::try_parse_from(["argmax", "init", shell]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidValue);
        }
    }

    #[test]
    fn incomplete_or_extra_syntax_fails_before_any_side_effect() {
        for arguments in [
            vec!["argmax", "config"],
            vec!["argmax", "init"],
            vec!["argmax", "version", "extra"],
            vec!["argmax", "crash-log", "--unknown"],
            vec!["argmax", "unknown"],
        ] {
            assert!(Invocation::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn help_and_parse_errors_do_not_enable_ansi_styling() {
        for arguments in [
            ["argmax", "--help"].as_slice(),
            ["argmax", "config", "--help"].as_slice(),
            ["argmax", "unknown"].as_slice(),
        ] {
            let output = Invocation::try_parse_from(arguments)
                .unwrap_err()
                .to_string();
            assert!(!output.contains('\u{1b}'));
        }
    }
}
