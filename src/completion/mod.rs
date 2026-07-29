//! Completion queries, candidates, tokenization, spec traversal, and merging.

mod merge;
mod model;
mod provider;
mod spec;
mod tokenize;

pub use merge::merge_suggestions;
pub use model::{CompletionQuery, InsertionBehavior, Suggestion, SuggestionSource, TextEdit};
pub use provider::{CancellationToken, CompletionProvider, ProviderBatch};
pub use spec::{CommandSpec, OptionSpec, SpecError, SpecIndex, SpecResolution};
pub use tokenize::{QuoteKind, ShellToken, TokenizedLine, tokenize};
