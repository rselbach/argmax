use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{CompletionQuery, Suggestion};

/// Cooperative cancellation shared by lookup and process providers.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Cancels current work. Cancellation is monotonic.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether work has lost authority.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// One independently failing provider response.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderBatch {
    /// Stable provider name for diagnostics.
    pub provider: &'static str,
    /// Query generation that produced these suggestions.
    pub generation: u64,
    /// Inert provider results.
    pub suggestions: Vec<Suggestion>,
    /// Sanitized provider failure; unrelated providers remain usable.
    pub error: Option<String>,
}

impl ProviderBatch {
    /// Successful response for a generation.
    #[must_use]
    pub fn success(provider: &'static str, generation: u64, suggestions: Vec<Suggestion>) -> Self {
        Self {
            provider,
            generation,
            suggestions,
            error: None,
        }
    }

    /// Independent provider failure for a generation.
    #[must_use]
    pub fn failure(provider: &'static str, generation: u64, error: impl Into<String>) -> Self {
        Self {
            provider,
            generation,
            suggestions: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// Provider boundary. Implementations cannot access terminal output.
pub trait CompletionProvider: Send + Sync {
    /// Stable diagnostic provider name.
    fn name(&self) -> &'static str;

    /// Computes inert suggestions for an immutable query snapshot.
    fn complete(&self, query: &CompletionQuery, cancellation: &CancellationToken) -> ProviderBatch;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_failures_are_data_not_panics() {
        let failure = ProviderBatch::failure("test", 3, "unavailable");
        assert_eq!(failure.error.as_deref(), Some("unavailable"));
        assert!(failure.suggestions.is_empty());
    }

    #[test]
    fn cancellation_is_shared_and_monotonic() {
        let first = CancellationToken::default();
        let second = first.clone();
        assert!(!second.is_cancelled());
        first.cancel();
        assert!(second.is_cancelled());
    }
}
