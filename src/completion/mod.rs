//! Completion queries, candidates, tokenization, spec traversal, and merging.

mod generator;
mod merge;
mod model;
mod provider;
mod spec;
mod tokenize;

/// Maximum bytes retained by [`ProviderBatch::failure`].
pub const MAX_PROVIDER_FAILURE_BYTES: usize = 8 * 1024;

pub use generator::{
    DEFAULT_CACHE_TTL, DEFAULT_FILESYSTEM_SCAN_LIMIT, DEFAULT_MAX_RESULTS, DEFAULT_TIMEOUT,
    FilesystemGenerator, GeneratorError, GeneratorKind, GeneratorSpec, GeneratorTarget,
    MAX_CACHE_TTL, MAX_EXTENSION_FILTERS, MAX_EXTENSION_LENGTH, MAX_FILESYSTEM_SCAN_LIMIT,
    MAX_RESULTS, MAX_TIMEOUT,
};
pub(crate) use merge::merge_suggestions;
pub use model::{CompletionQuery, InsertionBehavior, Suggestion, SuggestionSource, TextEdit};
pub use provider::{CancellationToken, CompletionProvider, ProviderBatch};
pub use spec::{CommandSpec, OptionSpec, SpecError, SpecIndex, SpecResolution};
pub use tokenize::{QuoteKind, ShellToken, TokenizedLine, tokenize};
