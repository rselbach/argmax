use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{CompletionQuery, MAX_PROVIDER_FAILURE_BYTES, Suggestion};

/// Cooperative cancellation shared by lookup and process providers.
///
/// This is an observer-only handle. Cancellation authority remains private to
/// the query coordinator, so a provider cannot cancel sibling work.
#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub(crate) fn observe(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled }
    }

    /// Reports whether work has lost authority.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// One independently failing provider response.
#[derive(Clone, PartialEq)]
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
    ///
    /// Terminal controls are removed or made visible, and retained text is
    /// truncated on a UTF-8 boundary to [`MAX_PROVIDER_FAILURE_BYTES`].
    #[must_use]
    pub fn failure(provider: &'static str, generation: u64, error: impl Into<String>) -> Self {
        let error = error.into();
        Self {
            provider,
            generation,
            suggestions: Vec::new(),
            error: Some(sanitize_failure(&error)),
        }
    }
}

impl std::fmt::Debug for ProviderBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderBatch")
            .field("provider_bytes", &self.provider.len())
            .field("generation", &self.generation)
            .field("suggestion_count", &self.suggestions.len())
            .field("has_error", &self.error.is_some())
            .finish()
    }
}

fn sanitize_failure(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(MAX_PROVIDER_FAILURE_BYTES));
    for character in value.chars() {
        let visible = match character {
            '\u{1b}' => continue,
            '\n' | '\r' | '\t' => ' ',
            character if character.is_control() => continue,
            character => character,
        };
        if visible.len_utf8() > MAX_PROVIDER_FAILURE_BYTES.saturating_sub(sanitized.len()) {
            break;
        }
        sanitized.push(visible);
    }
    sanitized.into_boxed_str().into_string()
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
    fn provider_failures_are_sanitized_bounded_and_redacted_in_debug() {
        let unsafe_error = format!(
            "hunter2\n\u{1b}[31m{}",
            "é".repeat(MAX_PROVIDER_FAILURE_BYTES)
        );
        let failure = ProviderBatch::failure("classified-provider", 3, unsafe_error);
        let error = failure.error.as_deref().unwrap();
        assert!(error.len() <= MAX_PROVIDER_FAILURE_BYTES);
        assert!(!error.chars().any(char::is_control));
        assert!(error.contains("hunter2 [31m"));
        let debug = format!("{failure:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("classified-provider"));
    }

    #[test]
    fn cancellation_observers_share_state_without_public_authority() {
        let state = Arc::new(AtomicBool::new(false));
        let first = CancellationToken::observe(Arc::clone(&state));
        let second = first.clone();
        assert!(!first.is_cancelled());
        assert!(!second.is_cancelled());

        state.store(true, Ordering::Release);
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
    }
}
