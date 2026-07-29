//! Local completion providers that cannot write to the terminal.

mod aliases;
mod cobra;
mod filesystem;
mod path;
mod workspace;

pub use aliases::{
    Alias, AliasLookup, alias_config_paths, alias_expansion_edit, alias_suggestions, load_aliases,
    parse_aliases, resolve_alias_for_lookup,
};
pub use cobra::{
    COBRA_COMPLETION_TIMEOUT, CobraBinaryIdentity, CobraCacheKey, CobraCandidate, CobraCompletion,
    CobraDirective, CobraExecutable, CobraFileCompletion, CobraProtocolError, CobraRequest,
    DIRECTIVE_ERROR, DIRECTIVE_FILTER_DIRECTORIES, DIRECTIVE_FILTER_FILE_EXTENSIONS,
    DIRECTIVE_KEEP_ORDER, DIRECTIVE_NO_FILE_COMPLETION, DIRECTIVE_NO_SPACE, KNOWN_DIRECTIVE_BITS,
    MAX_COBRA_CANDIDATES, MAX_COBRA_OUTPUT_BYTES, parse_cobra_output,
};
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
