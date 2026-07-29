//! Local completion providers that cannot write to the terminal.

mod aliases;
mod filesystem;
mod path;
mod workspace;

pub use aliases::{Alias, alias_config_paths, alias_suggestions, load_aliases, parse_aliases};
pub use filesystem::{FilesystemOptions, filesystem_suggestions, quote_path};
pub use path::{PathExecutable, discover_path_executables, path_suggestions};
pub use workspace::{WorkspaceContext, WorkspaceKind, WorkspaceSignature, detect_workspace};

/// Supported shell syntax relevant to local completion providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// Bourne Again Shell.
    Bash,
    /// Z shell.
    Zsh,
    /// Friendly Interactive Shell.
    Fish,
}
