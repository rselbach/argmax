//! Local completion providers that cannot write to the terminal.

mod aliases;
mod cache;
mod cobra;
mod dynamic;
mod filesystem;
mod path;
mod workspace;

pub use aliases::{
    Alias, AliasLookup, alias_config_paths, alias_expansion_edit, alias_suggestions, load_aliases,
    parse_aliases, resolve_alias_for_lookup,
};
pub use cache::{
    DynamicCacheAdmission, DynamicCacheKey, DynamicCacheKeyError, DynamicResultCache,
    MAX_DYNAMIC_CACHE_ARGUMENTS, MAX_DYNAMIC_CACHE_BYTES, MAX_DYNAMIC_CACHE_ENTRIES,
    MAX_DYNAMIC_CACHE_KEY_BYTES,
};
pub use cobra::{
    COBRA_COMPLETION_TIMEOUT, CobraBinaryIdentity, CobraCacheKey, CobraCandidate, CobraCompletion,
    CobraDirective, CobraExecutable, CobraFileCompletion, CobraProtocolError, CobraRequest,
    DIRECTIVE_ERROR, DIRECTIVE_FILTER_DIRECTORIES, DIRECTIVE_FILTER_FILE_EXTENSIONS,
    DIRECTIVE_KEEP_ORDER, DIRECTIVE_NO_FILE_COMPLETION, DIRECTIVE_NO_SPACE, KNOWN_DIRECTIVE_BITS,
    MAX_COBRA_CANDIDATES, MAX_COBRA_OUTPUT_BYTES, parse_cobra_output,
};
pub use dynamic::{
    DynamicItem, DynamicItemKind, DynamicMetadata, DynamicParseError, DynamicResourceKind,
    GitBranchOptions, GitBranchScope, MAX_DYNAMIC_ITEM_BYTES, MAX_DYNAMIC_ITEMS,
    MAX_DYNAMIC_OUTPUT_BYTES, environment_variable_items, parse_git_branches, parse_git_commits,
    parse_git_remotes, parse_git_stashes, parse_git_tags, parse_just_recipes, parse_make_targets,
    parse_processes, parse_resource_lines, parse_ssh_hosts, parse_zoxide_directories,
};
pub use filesystem::{FilesystemOptions, filesystem_suggestions, quote_path};
pub use path::{PathExecutable, PathExecutableCache, discover_path_executables, path_suggestions};
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
