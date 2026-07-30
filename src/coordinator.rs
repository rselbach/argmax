//! Bounded coordination of asynchronous completion-provider results.
//!
//! The coordinator owns query authority, cooperative cancellation, cumulative
//! provider results, full-set local ranking, and selection updates. Providers
//! receive immutable query snapshots and observer-only cancellation tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::completion::{
    CancellationToken, CompletionQuery, InsertionBehavior, MAX_PROVIDER_FAILURE_BYTES,
    ProviderBatch, Suggestion, SuggestionSource, merge_suggestions,
};
use crate::providers::{WorkspaceContext, WorkspaceKind};
use crate::ranking::{
    LocalRankingCandidate, LocalRankingContext, LocalRankingMetadata,
    rank_all_with_local_intelligence,
};
use crate::selection::{SelectionState, UpdateOutcome};

/// Maximum number of provider identities registered for one coordinator.
pub const MAX_REGISTERED_PROVIDERS: usize = 64;
/// Maximum UTF-8 byte length of one provider identity.
pub const MAX_PROVIDER_NAME_BYTES: usize = 128;
/// Maximum UTF-8 byte length of an authoritative shell buffer.
pub const MAX_QUERY_LINE_BYTES: usize = 256 * 1024;
/// Maximum encoded byte length of an authoritative working directory.
pub const MAX_QUERY_CWD_BYTES: usize = 16 * 1024;
/// Maximum number of candidates accepted from one provider batch.
pub const MAX_BATCH_CANDIDATES: usize = 500;
/// Maximum retained bytes represented by one candidate's owned text fields.
pub const MAX_CANDIDATE_BYTES: usize = 64 * 1024;
/// Maximum retained candidate bytes accepted in one provider batch.
pub const MAX_BATCH_BYTES: usize = 1024 * 1024;
/// Maximum bytes retained for one sanitized provider error.
pub const MAX_PROVIDER_ERROR_BYTES: usize = MAX_PROVIDER_FAILURE_BYTES;
/// Maximum candidates retained across all providers for one query.
pub const MAX_CUMULATIVE_CANDIDATES: usize = 4_096;
/// Maximum candidate and error bytes retained across providers for one query.
pub const MAX_CUMULATIVE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum bytes in one canonical command skeleton supplied for ranking.
pub const MAX_RANKING_SKELETON_BYTES: usize = 8 * 1024;
/// Maximum tokens in one canonical command skeleton supplied for ranking.
pub const MAX_RANKING_SKELETON_TOKENS: usize = 64;
/// Maximum canonical skeleton bytes admitted to one full-set ranking pass.
pub const MAX_RANKING_BYTES: usize = 4 * 1024 * 1024;
/// Maximum distinct workspace signatures admitted to ranking.
pub const MAX_WORKSPACE_SIGNATURES: usize = 9;
/// Maximum encoded byte length of one ranking workspace path.
pub const MAX_WORKSPACE_PATH_BYTES: usize = MAX_QUERY_CWD_BYTES;
/// Maximum validated UI result limit.
pub const MAX_UI_SUGGESTIONS: usize = 500;

/// Failure while registering provider identities or the UI result bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationError {
    /// More provider identities were supplied than the hard limit.
    TooManyProviders {
        /// Number observed before registration stopped.
        observed: usize,
        /// Hard provider limit.
        limit: usize,
    },
    /// A provider name was blank, padded, or contained control characters.
    InvalidProviderName {
        /// Registration position of the invalid name.
        index: usize,
    },
    /// A provider name exceeded the hard byte limit.
    ProviderNameTooLong {
        /// Registration position of the oversized name.
        index: usize,
        /// Observed UTF-8 byte length.
        bytes: usize,
        /// Hard provider-name byte limit.
        limit: usize,
    },
    /// The same exact provider identity was registered more than once.
    DuplicateProvider {
        /// Registration position of the repeated identity.
        index: usize,
        /// Duplicate identity length without retaining its text.
        provider_bytes: usize,
    },
    /// The UI result limit was outside one through [`MAX_UI_SUGGESTIONS`].
    InvalidUiLimit {
        /// Requested limit.
        value: usize,
        /// Largest supported limit.
        maximum: usize,
    },
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyProviders { observed, limit } => {
                write!(
                    formatter,
                    "registered {observed} providers; limit is {limit}"
                )
            }
            Self::InvalidProviderName { index } => {
                write!(formatter, "provider name at index {index} is invalid")
            }
            Self::ProviderNameTooLong {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "provider name at index {index} is {bytes} bytes; limit is {limit}"
            ),
            Self::DuplicateProvider {
                index,
                provider_bytes,
            } => {
                write!(
                    formatter,
                    "provider identity at index {index} is a duplicate ({provider_bytes} bytes)"
                )
            }
            Self::InvalidUiLimit { value, maximum } => write!(
                formatter,
                "UI result limit is {value}; expected 1 through {maximum}"
            ),
        }
    }
}

impl Error for RegistrationError {}

/// Failure to create a new authoritative query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryStartError {
    /// Every `u64` generation was issued; wrapping would resurrect stale work.
    GenerationExhausted,
    /// The shell buffer exceeded its hard byte limit.
    LineTooLarge {
        /// Observed UTF-8 bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// The working directory exceeded its hard encoded byte limit.
    CwdTooLarge {
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// The working directory was empty or relative.
    CwdNotAbsolute,
    /// The cursor was outside the line or split a UTF-8 code point.
    InvalidCursor {
        /// Supplied cursor byte offset.
        cursor: usize,
        /// Authoritative line length in bytes.
        line_bytes: usize,
    },
}

impl fmt::Display for QueryStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationExhausted => {
                formatter.write_str("completion query generation space is exhausted")
            }
            Self::LineTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "completion line is {bytes} bytes; limit is {limit}"
                )
            }
            Self::CwdTooLarge { bytes, limit } => write!(
                formatter,
                "completion working directory is {bytes} bytes; limit is {limit}"
            ),
            Self::CwdNotAbsolute => {
                formatter.write_str("completion working directory must be absolute")
            }
            Self::InvalidCursor { cursor, line_bytes } => write!(
                formatter,
                "cursor {cursor} is not a UTF-8 boundary in a {line_bytes}-byte line"
            ),
        }
    }
}

impl Error for QueryStartError {}

/// Immutable provider work issued for one query generation.
#[derive(Clone)]
pub struct QueryWork {
    query: Arc<CompletionQuery>,
    cancellation: CancellationToken,
}

impl QueryWork {
    /// Immutable query snapshot supplied to providers.
    #[must_use]
    pub fn query(&self) -> &CompletionQuery {
        &self.query
    }

    /// Observer-only cooperative cancellation token supplied to providers.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl fmt::Debug for QueryWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryWork")
            .field("generation", &self.query.generation)
            .field("cursor", &self.query.cursor)
            .field("line_bytes", &self.query.line.len())
            .field("cwd_bytes", &path_bytes(&self.query.cwd))
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

struct CancellationSource {
    cancelled: Arc<AtomicBool>,
}

impl CancellationSource {
    fn pair() -> (Self, CancellationToken) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let observer = CancellationToken::observe(Arc::clone(&cancelled));
        (Self { cancelled }, observer)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for CancellationSource {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Result of explicitly cancelling active provider work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum CancellationOutcome {
    /// No active query existed.
    NoActiveQuery,
    /// The active generation was cancelled and its cumulative state was dropped.
    Cancelled {
        /// Cancelled generation.
        generation: u64,
    },
}

/// Why work has no authority to inspect or change the current query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityRejection {
    /// No query is active.
    NoActiveQuery,
    /// The named generation was explicitly cancelled.
    Cancelled {
        /// Cancelled generation.
        generation: u64,
    },
    /// A different generation is active.
    GenerationMismatch {
        /// Active generation.
        active: u64,
        /// Generation supplied by the caller or provider.
        received: u64,
    },
}

impl fmt::Display for AuthorityRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveQuery => formatter.write_str("no completion query is active"),
            Self::Cancelled { generation } => {
                write!(
                    formatter,
                    "completion generation {generation} was cancelled"
                )
            }
            Self::GenerationMismatch { active, received } => write!(
                formatter,
                "completion generation {received} is stale or premature; active generation is {active}"
            ),
        }
    }
}

impl Error for AuthorityRejection {}

/// Accepted provider state for the active query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderPhase {
    /// The provider has not submitted a batch for this query.
    Pending,
    /// The provider most recently submitted a successful batch.
    Ready,
    /// The provider most recently failed and currently contributes no candidates.
    Failed,
}

/// Read-only provider state for diagnostics.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ProviderDiagnostic<'a> {
    provider: &'static str,
    phase: ProviderPhase,
    candidate_count: usize,
    error: Option<&'a str>,
}

impl fmt::Debug for ProviderDiagnostic<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderDiagnostic")
            .field("provider_bytes", &self.provider.len())
            .field("phase", &self.phase)
            .field("candidate_count", &self.candidate_count)
            .field("error_bytes", &self.error.map(str::len))
            .finish()
    }
}

impl<'a> ProviderDiagnostic<'a> {
    /// Registered provider identity.
    #[must_use]
    pub const fn provider(&self) -> &'static str {
        self.provider
    }

    /// Current phase for the active query.
    #[must_use]
    pub const fn phase(&self) -> ProviderPhase {
        self.phase
    }

    /// Number of candidates currently contributed by this provider.
    #[must_use]
    pub const fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    /// Bounded sanitized error from the latest failed batch.
    #[must_use]
    pub const fn error(&self) -> Option<&'a str> {
        self.error
    }
}

/// Whether an accepted batch represents success or isolated provider failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptedBatchKind {
    /// Successful provider candidates replaced its prior contribution.
    Success,
    /// Provider failure replaced only that provider's contribution with no items.
    Failure,
}

/// Inspectable facts about one accepted provider replacement.
#[derive(Clone, Eq, PartialEq)]
pub struct BatchAcceptance {
    provider: &'static str,
    generation: u64,
    kind: AcceptedBatchKind,
    replaced_previous: bool,
    provider_candidates: usize,
    cumulative_candidates: usize,
}

impl fmt::Debug for BatchAcceptance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BatchAcceptance")
            .field("provider_bytes", &self.provider.len())
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("replaced_previous", &self.replaced_previous)
            .field("provider_candidates", &self.provider_candidates)
            .field("cumulative_candidates", &self.cumulative_candidates)
            .finish()
    }
}

impl BatchAcceptance {
    /// Provider whose cumulative contribution was replaced.
    #[must_use]
    pub const fn provider(&self) -> &'static str {
        self.provider
    }

    /// Exact accepted query generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Success or isolated provider failure.
    #[must_use]
    pub const fn kind(&self) -> AcceptedBatchKind {
        self.kind
    }

    /// Whether this provider had already submitted a batch for the query.
    #[must_use]
    pub const fn replaced_previous(&self) -> bool {
        self.replaced_previous
    }

    /// Candidates now retained for this provider.
    #[must_use]
    pub const fn provider_candidates(&self) -> usize {
        self.provider_candidates
    }

    /// Candidates retained across all providers before merge deduplication.
    #[must_use]
    pub const fn cumulative_candidates(&self) -> usize {
        self.cumulative_candidates
    }
}

/// Why a provider batch was rejected without changing cumulative or selection state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchRejection {
    /// Query authority was absent, cancelled, or belonged to another generation.
    Authority(AuthorityRejection),
    /// The provider identity was not registered.
    UnknownProvider {
        /// Unrecognized provider identity length without retaining its text.
        provider_bytes: usize,
    },
    /// A batch claimed both candidates and failure.
    ConflictingSuccessAndFailure,
    /// A provider error exceeded its byte bound.
    ErrorTooLarge {
        /// Observed UTF-8 byte length.
        bytes: usize,
        /// Hard error limit.
        limit: usize,
    },
    /// A provider error contained a terminal or other control character.
    UnsafeErrorText,
    /// A batch contained too many candidates.
    TooManyCandidates {
        /// Observed candidate count.
        observed: usize,
        /// Hard per-batch count limit.
        limit: usize,
    },
    /// One candidate exceeded its owned-text byte bound.
    CandidateTooLarge {
        /// Zero-based candidate position in the submitted batch.
        index: usize,
        /// Observed owned-text bytes.
        bytes: usize,
        /// Hard per-candidate limit.
        limit: usize,
    },
    /// Aggregate candidate bytes in this batch exceeded the bound.
    BatchTooLarge {
        /// Observed bytes, saturated at [`usize::MAX`].
        bytes: usize,
        /// Hard batch-byte limit.
        limit: usize,
    },
    /// Replacing this provider would exceed the cumulative candidate count.
    TooManyCumulativeCandidates {
        /// Prospective retained candidate count.
        observed: usize,
        /// Hard cumulative count limit.
        limit: usize,
    },
    /// Replacing this provider would exceed the cumulative retained byte bound.
    CumulativeBytesTooLarge {
        /// Prospective retained bytes, saturated at [`usize::MAX`].
        bytes: usize,
        /// Hard cumulative byte limit.
        limit: usize,
    },
}

impl fmt::Display for BatchRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(error) => write!(formatter, "{error}"),
            Self::UnknownProvider { provider_bytes } => {
                write!(
                    formatter,
                    "unregistered completion provider identity is {provider_bytes} bytes"
                )
            }
            Self::ConflictingSuccessAndFailure => {
                formatter.write_str("provider batch contains candidates and an error")
            }
            Self::ErrorTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "provider error is {bytes} bytes; limit is {limit}"
                )
            }
            Self::UnsafeErrorText => {
                formatter.write_str("provider error contains an unsafe control character")
            }
            Self::TooManyCandidates { observed, limit } => write!(
                formatter,
                "provider batch contains {observed} candidates; limit is {limit}"
            ),
            Self::CandidateTooLarge {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "candidate {index} contains {bytes} owned-text bytes; limit is {limit}"
            ),
            Self::BatchTooLarge { bytes, limit } => write!(
                formatter,
                "provider batch contains {bytes} bytes; limit is {limit}"
            ),
            Self::TooManyCumulativeCandidates { observed, limit } => write!(
                formatter,
                "query would retain {observed} provider candidates; limit is {limit}"
            ),
            Self::CumulativeBytesTooLarge { bytes, limit } => write!(
                formatter,
                "query would retain {bytes} provider bytes; limit is {limit}"
            ),
        }
    }
}

impl Error for BatchRejection {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Authority(error) => Some(error),
            _ => None,
        }
    }
}

/// Accepted or rejected disposition for one provider response.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use]
pub enum BatchOutcome {
    /// The provider's cumulative state was replaced.
    Accepted(BatchAcceptance),
    /// Nothing changed.
    Rejected(BatchRejection),
}

/// Why a full ranked candidate list could not be applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresentationRejection {
    /// Query authority was absent, cancelled, or belonged to another generation.
    Authority(AuthorityRejection),
    /// The ranked list exceeded the cumulative candidate hard bound.
    TooManyCandidates {
        /// Ranked candidate count.
        observed: usize,
        /// Hard count limit.
        limit: usize,
    },
    /// One ranked candidate exceeded the owned-text byte bound.
    CandidateTooLarge {
        /// Zero-based position in ranked order.
        index: usize,
        /// Observed owned-text bytes.
        bytes: usize,
        /// Hard per-candidate limit.
        limit: usize,
    },
    /// Aggregate ranked-candidate bytes exceeded the cumulative hard bound.
    CumulativeBytesTooLarge {
        /// Observed bytes, saturated at [`usize::MAX`].
        bytes: usize,
        /// Hard cumulative byte limit.
        limit: usize,
    },
    /// One canonical ranking skeleton exceeded its byte bound.
    RankingSkeletonTooLarge {
        /// Zero-based merged candidate position.
        index: usize,
        /// Observed UTF-8 bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// One candidate skeleton was not canonical single-space-separated text.
    InvalidRankingSkeleton {
        /// Zero-based merged candidate position.
        index: usize,
    },
    /// One candidate skeleton exceeded the canonical token-count bound.
    TooManyRankingSkeletonTokens {
        /// Zero-based merged candidate position.
        index: usize,
        /// Tokens observed before validation stopped.
        observed: usize,
        /// Hard token-count limit.
        limit: usize,
    },
    /// Aggregate canonical ranking skeleton bytes exceeded the pass bound.
    RankingMetadataTooLarge {
        /// Observed bytes, saturated at [`usize::MAX`].
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// The prior skeleton exceeded its hard byte bound.
    PriorSkeletonTooLarge {
        /// Observed UTF-8 bytes.
        bytes: usize,
        /// Hard byte limit.
        limit: usize,
    },
    /// The prior skeleton was not canonical single-space-separated text.
    InvalidPriorSkeleton,
    /// The prior skeleton exceeded the canonical token-count bound.
    TooManyPriorSkeletonTokens {
        /// Tokens observed before validation stopped.
        observed: usize,
        /// Hard token-count limit.
        limit: usize,
    },
    /// The workspace detection origin exceeded its encoded byte bound.
    WorkspaceCwdTooLarge {
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard path byte limit.
        limit: usize,
    },
    /// The workspace detection origin was empty or relative.
    WorkspaceCwdNotAbsolute,
    /// The workspace detection origin did not match the authoritative query.
    WorkspaceCwdMismatch,
    /// Workspace context contained more than one signature per supported kind.
    TooManyWorkspaceSignatures {
        /// Submitted signature count.
        observed: usize,
        /// Hard signature count limit.
        limit: usize,
    },
    /// Workspace context repeated an ecosystem kind.
    DuplicateWorkspaceKind {
        /// Zero-based duplicate signature position.
        index: usize,
        /// Repeated ecosystem kind.
        kind: WorkspaceKind,
    },
    /// A workspace signature root was empty or relative.
    WorkspaceRootNotAbsolute {
        /// Zero-based signature position.
        index: usize,
    },
    /// A workspace signature root exceeded its encoded byte bound.
    WorkspaceRootTooLarge {
        /// Zero-based signature position.
        index: usize,
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard path byte limit.
        limit: usize,
    },
    /// A workspace signature marker was empty or relative.
    WorkspaceMarkerNotAbsolute {
        /// Zero-based signature position.
        index: usize,
    },
    /// A workspace signature marker exceeded its encoded byte bound.
    WorkspaceMarkerTooLarge {
        /// Zero-based signature position.
        index: usize,
        /// Observed encoded bytes.
        bytes: usize,
        /// Hard path byte limit.
        limit: usize,
    },
    /// Ranked candidates were not an exact permutation of the current merged set.
    CandidateSetMismatch,
    /// Selection and coordinator generations unexpectedly diverged.
    SelectionGenerationMismatch,
}

impl fmt::Display for PresentationRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(error) => write!(formatter, "{error}"),
            Self::TooManyCandidates { observed, limit } => write!(
                formatter,
                "ranked result contains {observed} candidates; limit is {limit}"
            ),
            Self::CandidateTooLarge {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "ranked candidate {index} contains {bytes} owned-text bytes; limit is {limit}"
            ),
            Self::CumulativeBytesTooLarge { bytes, limit } => write!(
                formatter,
                "ranked candidates contain {bytes} owned-text bytes; limit is {limit}"
            ),
            Self::RankingSkeletonTooLarge {
                index,
                bytes,
                limit,
            } => write!(
                formatter,
                "ranking skeleton {index} is {bytes} bytes; limit is {limit}"
            ),
            Self::InvalidRankingSkeleton { index } => write!(
                formatter,
                "ranking skeleton {index} is not canonical single-space-separated text"
            ),
            Self::TooManyRankingSkeletonTokens {
                index,
                observed,
                limit,
            } => write!(
                formatter,
                "ranking skeleton {index} contains at least {observed} tokens; limit is {limit}"
            ),
            Self::RankingMetadataTooLarge { bytes, limit } => write!(
                formatter,
                "ranking metadata contains {bytes} bytes; limit is {limit}"
            ),
            Self::PriorSkeletonTooLarge { bytes, limit } => write!(
                formatter,
                "prior ranking skeleton is {bytes} bytes; limit is {limit}"
            ),
            Self::InvalidPriorSkeleton => formatter
                .write_str("prior ranking skeleton is not canonical single-space-separated text"),
            Self::TooManyPriorSkeletonTokens { observed, limit } => write!(
                formatter,
                "prior ranking skeleton contains at least {observed} tokens; limit is {limit}"
            ),
            rejection @ (Self::WorkspaceCwdTooLarge { .. }
            | Self::WorkspaceCwdNotAbsolute
            | Self::WorkspaceCwdMismatch
            | Self::TooManyWorkspaceSignatures { .. }
            | Self::DuplicateWorkspaceKind { .. }
            | Self::WorkspaceRootNotAbsolute { .. }
            | Self::WorkspaceRootTooLarge { .. }
            | Self::WorkspaceMarkerNotAbsolute { .. }
            | Self::WorkspaceMarkerTooLarge { .. }) => {
                format_workspace_rejection(rejection, formatter)
            }
            Self::CandidateSetMismatch => formatter.write_str(
                "ranked candidates are not an exact permutation of the merged candidate set",
            ),
            Self::SelectionGenerationMismatch => {
                formatter.write_str("selection generation does not match the active query")
            }
        }
    }
}

fn format_workspace_rejection(
    rejection: &PresentationRejection,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    match rejection {
        PresentationRejection::WorkspaceCwdTooLarge { bytes, limit } => write!(
            formatter,
            "workspace detection origin is {bytes} bytes; limit is {limit}"
        ),
        PresentationRejection::WorkspaceCwdNotAbsolute => {
            formatter.write_str("workspace detection origin must be absolute")
        }
        PresentationRejection::WorkspaceCwdMismatch => {
            formatter.write_str("workspace detection origin does not match the active query")
        }
        PresentationRejection::TooManyWorkspaceSignatures { observed, limit } => write!(
            formatter,
            "workspace context contains {observed} signatures; limit is {limit}"
        ),
        PresentationRejection::DuplicateWorkspaceKind { index, kind } => write!(
            formatter,
            "workspace signature {index} repeats ecosystem {kind:?}"
        ),
        PresentationRejection::WorkspaceRootNotAbsolute { index } => write!(
            formatter,
            "workspace signature {index} root must be absolute"
        ),
        PresentationRejection::WorkspaceRootTooLarge {
            index,
            bytes,
            limit,
        } => write!(
            formatter,
            "workspace signature {index} root is {bytes} bytes; limit is {limit}"
        ),
        PresentationRejection::WorkspaceMarkerNotAbsolute { index } => write!(
            formatter,
            "workspace signature {index} marker must be absolute"
        ),
        PresentationRejection::WorkspaceMarkerTooLarge {
            index,
            bytes,
            limit,
        } => write!(
            formatter,
            "workspace signature {index} marker is {bytes} bytes; limit is {limit}"
        ),
        _ => formatter.write_str("invalid ranking workspace context"),
    }
}

impl Error for PresentationRejection {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Authority(error) => Some(error),
            _ => None,
        }
    }
}

/// Result of applying full-set ranking followed by the UI limit.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use]
pub enum PresentationOutcome {
    /// Ranked candidates were applied to selection.
    Applied {
        /// Candidate count before the UI limit.
        available: usize,
        /// Candidate count in the authoritative rendering window.
        displayed: usize,
    },
    /// Navigation froze an edit missing from the new full candidate set.
    SelectionConflict {
        /// Candidate count before the UI limit.
        available: usize,
        /// Candidate count the update would otherwise display.
        displayed: usize,
    },
    /// Selection state was unchanged.
    Rejected(PresentationRejection),
}

struct StoredBatch {
    suggestions: Box<[Suggestion]>,
    error: Option<Box<str>>,
    retained_bytes: usize,
}

impl StoredBatch {
    const fn phase(&self) -> ProviderPhase {
        if self.error.is_some() {
            ProviderPhase::Failed
        } else {
            ProviderPhase::Ready
        }
    }
}

struct ActiveQuery {
    query: Arc<CompletionQuery>,
    cancellation: CancellationSource,
    batches: BTreeMap<&'static str, StoredBatch>,
    retained_candidates: usize,
    retained_bytes: usize,
}

/// Pure coordinator for one session's asynchronous completion queries.
///
/// The type intentionally does not implement [`Clone`]: cloning it would fork
/// generation and cancellation authority.
pub struct CompletionCoordinator {
    providers: BTreeSet<&'static str>,
    ui_max_suggestions: usize,
    next_generation: Option<u64>,
    active: Option<ActiveQuery>,
    last_cancelled_generation: Option<u64>,
    selection: SelectionState,
}

impl fmt::Debug for CompletionCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active_generation = self.active.as_ref().map(|active| active.query.generation);
        let active_line_bytes = self.active.as_ref().map(|active| active.query.line.len());
        let active_cwd_bytes = self
            .active
            .as_ref()
            .map(|active| path_bytes(&active.query.cwd));
        let retained_candidates = self
            .active
            .as_ref()
            .map_or(0, |active| active.retained_candidates);
        let retained_bytes = self
            .active
            .as_ref()
            .map_or(0, |active| active.retained_bytes);

        formatter
            .debug_struct("CompletionCoordinator")
            .field("provider_count", &self.providers.len())
            .field("ui_max_suggestions", &self.ui_max_suggestions)
            .field("next_generation", &self.next_generation)
            .field("active_generation", &active_generation)
            .field("active_line_bytes", &active_line_bytes)
            .field("active_cwd_bytes", &active_cwd_bytes)
            .field("retained_candidates", &retained_candidates)
            .field("retained_bytes", &retained_bytes)
            .field("last_cancelled_generation", &self.last_cancelled_generation)
            .field("displayed_candidates", &self.selection.candidates().len())
            .finish()
    }
}

impl CompletionCoordinator {
    /// Registers exact provider identities and validates the UI result limit.
    ///
    /// # Errors
    ///
    /// Returns a registration error for an invalid, duplicate, or excessive
    /// provider set, or an invalid UI limit.
    pub fn new(
        providers: impl IntoIterator<Item = &'static str>,
        ui_max_suggestions: usize,
    ) -> Result<Self, RegistrationError> {
        Self::with_next_generation(providers, ui_max_suggestions, Some(0))
    }

    /// Applies a new validated UI result limit and cancels current work.
    ///
    /// The next authoritative buffer snapshot starts a fresh generation under
    /// the new bound, so an old oversized selection cannot remain visible.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::InvalidUiLimit`] without changing state
    /// when `ui_max_suggestions` is outside one through 500.
    pub fn reconfigure_ui_limit(
        &mut self,
        ui_max_suggestions: usize,
    ) -> Result<(), RegistrationError> {
        if !(1..=MAX_UI_SUGGESTIONS).contains(&ui_max_suggestions) {
            return Err(RegistrationError::InvalidUiLimit {
                value: ui_max_suggestions,
                maximum: MAX_UI_SUGGESTIONS,
            });
        }
        self.ui_max_suggestions = ui_max_suggestions;
        self.abandon_active_query();
        self.clear_selection();
        Ok(())
    }

    fn with_next_generation(
        providers: impl IntoIterator<Item = &'static str>,
        ui_max_suggestions: usize,
        next_generation: Option<u64>,
    ) -> Result<Self, RegistrationError> {
        if !(1..=MAX_UI_SUGGESTIONS).contains(&ui_max_suggestions) {
            return Err(RegistrationError::InvalidUiLimit {
                value: ui_max_suggestions,
                maximum: MAX_UI_SUGGESTIONS,
            });
        }

        let mut registered = BTreeSet::new();
        for (index, provider) in providers.into_iter().enumerate() {
            let observed = index.saturating_add(1);
            if observed > MAX_REGISTERED_PROVIDERS {
                return Err(RegistrationError::TooManyProviders {
                    observed,
                    limit: MAX_REGISTERED_PROVIDERS,
                });
            }
            if provider.is_empty()
                || provider.trim() != provider
                || provider.chars().any(char::is_control)
            {
                return Err(RegistrationError::InvalidProviderName { index });
            }
            if provider.len() > MAX_PROVIDER_NAME_BYTES {
                return Err(RegistrationError::ProviderNameTooLong {
                    index,
                    bytes: provider.len(),
                    limit: MAX_PROVIDER_NAME_BYTES,
                });
            }
            if !registered.insert(provider) {
                return Err(RegistrationError::DuplicateProvider {
                    index,
                    provider_bytes: provider.len(),
                });
            }
        }

        Ok(Self {
            providers: registered,
            ui_max_suggestions,
            next_generation,
            active: None,
            last_cancelled_generation: None,
            selection: SelectionState::default(),
        })
    }

    /// Starts a new immutable query after cancelling and discarding prior work.
    ///
    /// Generation values are never reused. Once `u64::MAX` has been issued, a
    /// later start fails closed instead of wrapping to zero. Line and path
    /// allocations are compacted before the query is retained.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation error or generation exhaustion. Prior work
    /// is cancelled even when the new query cannot start.
    pub fn start_query(
        &mut self,
        line: impl Into<String>,
        cursor: usize,
        cwd: impl Into<PathBuf>,
    ) -> Result<QueryWork, QueryStartError> {
        let line = line.into();
        let cwd = cwd.into();
        self.abandon_active_query();

        let Some(generation) = self.take_generation() else {
            self.clear_selection();
            return Err(QueryStartError::GenerationExhausted);
        };
        self.selection.begin_query(generation, Vec::new());

        if line.len() > MAX_QUERY_LINE_BYTES {
            self.last_cancelled_generation = Some(generation);
            return Err(QueryStartError::LineTooLarge {
                bytes: line.len(),
                limit: MAX_QUERY_LINE_BYTES,
            });
        }
        let cwd_bytes = path_bytes(&cwd);
        if cwd_bytes > MAX_QUERY_CWD_BYTES {
            self.last_cancelled_generation = Some(generation);
            return Err(QueryStartError::CwdTooLarge {
                bytes: cwd_bytes,
                limit: MAX_QUERY_CWD_BYTES,
            });
        }
        if !cwd.is_absolute() {
            self.last_cancelled_generation = Some(generation);
            return Err(QueryStartError::CwdNotAbsolute);
        }
        if !line.is_char_boundary(cursor) {
            self.last_cancelled_generation = Some(generation);
            return Err(QueryStartError::InvalidCursor {
                cursor,
                line_bytes: line.len(),
            });
        }

        let query = Arc::new(CompletionQuery {
            line: compact_string(line),
            cursor,
            cwd: compact_path(cwd),
            generation,
        });
        let (cancellation, observer) = CancellationSource::pair();
        self.active = Some(ActiveQuery {
            query: Arc::clone(&query),
            cancellation,
            batches: BTreeMap::new(),
            retained_candidates: 0,
            retained_bytes: 0,
        });
        self.last_cancelled_generation = None;

        Ok(QueryWork {
            query,
            cancellation: observer,
        })
    }

    /// Cancels active work, clears provider state, and hides current candidates.
    pub fn cancel_active_query(&mut self) -> CancellationOutcome {
        self.abandon_active_query()
            .map_or(CancellationOutcome::NoActiveQuery, |generation| {
                CancellationOutcome::Cancelled { generation }
            })
    }

    /// Returns the active immutable query, if provider work remains authoritative.
    #[must_use]
    pub fn active_query(&self) -> Option<&CompletionQuery> {
        self.active
            .as_ref()
            .filter(|active| !active.cancellation.is_cancelled())
            .map(|active| active.query.as_ref())
    }

    /// Returns selection and its authoritative bounded rendering candidates.
    #[must_use]
    pub const fn selection(&self) -> &SelectionState {
        &self.selection
    }

    /// Moves selection up without wrapping and freezes its edit semantics.
    pub fn select_previous(&mut self) {
        self.selection.up();
    }

    /// Moves selection down without wrapping and freezes its edit semantics.
    pub fn select_next(&mut self) {
        self.selection.down();
    }

    /// Hides menu and ghost text until the authoritative buffer changes.
    pub fn dismiss_suggestions(&mut self) {
        self.selection.escape();
    }

    /// Makes suggestions eligible after an authoritative buffer change.
    pub fn note_buffer_changed(&mut self) {
        self.selection.buffer_changed();
    }

    /// Toggles the suggestion layer for the current session.
    pub fn toggle_suggestion_layer(&mut self) {
        self.selection.toggle_visible();
    }

    /// Returns bounded provider diagnostics in deterministic lexical order.
    #[must_use]
    pub fn provider_diagnostics(&self) -> Vec<ProviderDiagnostic<'_>> {
        self.providers
            .iter()
            .map(|provider| {
                self.active
                    .as_ref()
                    .and_then(|active| active.batches.get(provider))
                    .map_or(
                        ProviderDiagnostic {
                            provider,
                            phase: ProviderPhase::Pending,
                            candidate_count: 0,
                            error: None,
                        },
                        |batch| ProviderDiagnostic {
                            provider,
                            phase: batch.phase(),
                            candidate_count: batch.suggestions.len(),
                            error: batch.error.as_deref(),
                        },
                    )
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into_vec()
    }

    /// Accepts one exact-generation provider replacement without ranking it.
    ///
    /// A successful batch replaces that provider's prior candidates. A failed
    /// batch clears only that provider's candidates and retains a bounded error.
    /// Every rejection leaves cumulative and selection state unchanged.
    pub fn accept_batch(&mut self, batch: ProviderBatch) -> BatchOutcome {
        let provider = batch.provider;
        let generation = batch.generation;
        if let Err(error) = self.authority(generation) {
            return BatchOutcome::Rejected(BatchRejection::Authority(error));
        }
        if !self.providers.contains(provider) {
            return BatchOutcome::Rejected(BatchRejection::UnknownProvider {
                provider_bytes: provider.len(),
            });
        }
        let stored = match validate_batch(batch) {
            Ok(stored) => stored,
            Err(error) => return BatchOutcome::Rejected(error),
        };

        let active = match self.authority(generation) {
            Ok(active) => active,
            Err(error) => {
                return BatchOutcome::Rejected(BatchRejection::Authority(error));
            }
        };
        let previous = active.batches.get(provider);
        let previous_candidates = previous.map_or(0, |value| value.suggestions.len());
        let previous_bytes = previous.map_or(0, |value| value.retained_bytes);
        let prospective_candidates = active
            .retained_candidates
            .saturating_sub(previous_candidates)
            .saturating_add(stored.suggestions.len());
        if prospective_candidates > MAX_CUMULATIVE_CANDIDATES {
            return BatchOutcome::Rejected(BatchRejection::TooManyCumulativeCandidates {
                observed: prospective_candidates,
                limit: MAX_CUMULATIVE_CANDIDATES,
            });
        }
        let prospective_bytes = active
            .retained_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(stored.retained_bytes);
        if prospective_bytes > MAX_CUMULATIVE_BYTES {
            return BatchOutcome::Rejected(BatchRejection::CumulativeBytesTooLarge {
                bytes: prospective_bytes,
                limit: MAX_CUMULATIVE_BYTES,
            });
        }

        let replaced_previous = previous.is_some();
        let kind = if stored.error.is_some() {
            AcceptedBatchKind::Failure
        } else {
            AcceptedBatchKind::Success
        };
        let provider_candidates = stored.suggestions.len();
        let Some(active) = self.active.as_mut() else {
            return BatchOutcome::Rejected(BatchRejection::Authority(
                AuthorityRejection::NoActiveQuery,
            ));
        };
        active.batches.insert(provider, stored);
        active.retained_candidates = prospective_candidates;
        active.retained_bytes = prospective_bytes;

        BatchOutcome::Accepted(BatchAcceptance {
            provider,
            generation,
            kind,
            replaced_previous,
            provider_candidates,
            cumulative_candidates: prospective_candidates,
        })
    }

    /// Merges every accepted provider contribution for an exact generation.
    ///
    /// No UI limit or ranking is applied.
    ///
    /// # Errors
    ///
    /// Returns an authority rejection for stale, cancelled, or absent work.
    pub fn merged_candidates(
        &self,
        generation: u64,
    ) -> Result<Vec<Suggestion>, AuthorityRejection> {
        let active = self.authority(generation)?;
        Ok(merged_for(active))
    }

    /// Applies a caller-ranked full permutation after exact validation.
    ///
    /// This path supports modes with their own documented ranking rules, such as
    /// history. Spec-mode callers should use [`Self::rank_and_apply`] so local
    /// intelligence is applied by the coordinator itself.
    pub fn apply_ranked(
        &mut self,
        generation: u64,
        ranked: Vec<Suggestion>,
    ) -> PresentationOutcome {
        let merged = match self.authority(generation) {
            Ok(active) => merged_for(active),
            Err(error) => {
                return PresentationOutcome::Rejected(PresentationRejection::Authority(error));
            }
        };
        self.apply_ranked_against(generation, &merged, ranked)
    }

    /// Merges, ranks the complete set with local intelligence, then applies the UI limit.
    ///
    /// The metadata callback cannot add, remove, or replace a suggestion. It is
    /// called exactly once per merged candidate to supply a bounded canonical
    /// skeleton and match quality. The real composite ranker returns a full list,
    /// which is verified as an exact permutation before selection changes.
    pub fn rank_and_apply(
        &mut self,
        generation: u64,
        context: LocalRankingContext<'_>,
        mut metadata_for: impl FnMut(&Suggestion) -> LocalRankingMetadata,
    ) -> PresentationOutcome {
        let (query, merged) = match self.authority(generation) {
            Ok(active) => (Arc::clone(&active.query), merged_for(active)),
            Err(error) => {
                return PresentationOutcome::Rejected(PresentationRejection::Authority(error));
            }
        };

        if let Err(error) = validate_ranking_context(&query, context) {
            return PresentationOutcome::Rejected(error);
        }

        let mut metadata_bytes = 0usize;
        let mut candidates = Vec::with_capacity(merged.len());
        for (index, suggestion) in merged.iter().enumerate() {
            let metadata = metadata_for(suggestion);
            if metadata.skeleton.len() > MAX_RANKING_SKELETON_BYTES {
                return PresentationOutcome::Rejected(
                    PresentationRejection::RankingSkeletonTooLarge {
                        index,
                        bytes: metadata.skeleton.len(),
                        limit: MAX_RANKING_SKELETON_BYTES,
                    },
                );
            }
            match validate_canonical_skeleton(&metadata.skeleton) {
                Ok(()) => {}
                Err(SkeletonRejection::Invalid) => {
                    return PresentationOutcome::Rejected(
                        PresentationRejection::InvalidRankingSkeleton { index },
                    );
                }
                Err(SkeletonRejection::TooManyTokens { observed }) => {
                    return PresentationOutcome::Rejected(
                        PresentationRejection::TooManyRankingSkeletonTokens {
                            index,
                            observed,
                            limit: MAX_RANKING_SKELETON_TOKENS,
                        },
                    );
                }
            }
            metadata_bytes = metadata_bytes.saturating_add(metadata.skeleton.len());
            if metadata_bytes > MAX_RANKING_BYTES {
                return PresentationOutcome::Rejected(
                    PresentationRejection::RankingMetadataTooLarge {
                        bytes: metadata_bytes,
                        limit: MAX_RANKING_BYTES,
                    },
                );
            }
            candidates.push(LocalRankingCandidate::new(
                suggestion.clone(),
                metadata.skeleton,
                metadata.match_quality,
            ));
        }

        let context = LocalRankingContext {
            workspace: context.workspace,
            learning: context.learning,
            cwd: &query.cwd,
            now: context.now,
            prior_skeleton: context.prior_skeleton,
        };
        let ranked =
            rank_all_with_local_intelligence(candidates.into_boxed_slice().into_vec(), context)
                .candidates
                .into_iter()
                .map(|candidate| candidate.suggestion)
                .collect();

        self.apply_ranked_against(generation, &merged, ranked)
    }

    fn apply_ranked_against(
        &mut self,
        generation: u64,
        merged: &[Suggestion],
        ranked: Vec<Suggestion>,
    ) -> PresentationOutcome {
        let ranked = match validate_and_normalize_ranked_candidates(merged, ranked) {
            Ok(ranked) => ranked,
            Err(error) => return PresentationOutcome::Rejected(error),
        };

        let available = ranked.len();
        let displayed = available.min(self.ui_max_suggestions);
        match self
            .selection
            .apply_ranked_update(generation, ranked, self.ui_max_suggestions)
        {
            UpdateOutcome::Applied => PresentationOutcome::Applied {
                available,
                displayed: self.selection.candidates().len(),
            },
            UpdateOutcome::SelectionConflict => PresentationOutcome::SelectionConflict {
                available,
                displayed,
            },
            UpdateOutcome::Stale => {
                PresentationOutcome::Rejected(PresentationRejection::SelectionGenerationMismatch)
            }
        }
    }

    fn authority(&self, generation: u64) -> Result<&ActiveQuery, AuthorityRejection> {
        let Some(active) = &self.active else {
            return Err(self.inactive_rejection(generation));
        };
        if generation != active.query.generation {
            return Err(AuthorityRejection::GenerationMismatch {
                active: active.query.generation,
                received: generation,
            });
        }
        if active.cancellation.is_cancelled() {
            return Err(AuthorityRejection::Cancelled { generation });
        }
        Ok(active)
    }

    fn inactive_rejection(&self, generation: u64) -> AuthorityRejection {
        if self.last_cancelled_generation == Some(generation) {
            AuthorityRejection::Cancelled { generation }
        } else {
            AuthorityRejection::NoActiveQuery
        }
    }

    fn take_generation(&mut self) -> Option<u64> {
        let generation = self.next_generation?;
        self.next_generation = generation.checked_add(1);
        Some(generation)
    }

    fn abandon_active_query(&mut self) -> Option<u64> {
        let active = self.active.take()?;
        let generation = active.query.generation;
        active.cancellation.cancel();
        self.last_cancelled_generation = Some(generation);
        self.clear_selection();
        Some(generation)
    }

    fn clear_selection(&mut self) {
        self.selection
            .begin_query(self.selection.generation(), Vec::new());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkeletonRejection {
    Invalid,
    TooManyTokens { observed: usize },
}

fn validate_canonical_skeleton(skeleton: &str) -> Result<(), SkeletonRejection> {
    let mut token_count = 0usize;
    for token in skeleton.split(' ') {
        if token.is_empty()
            || token
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(SkeletonRejection::Invalid);
        }
        token_count = token_count.saturating_add(1);
        if token_count > MAX_RANKING_SKELETON_TOKENS {
            return Err(SkeletonRejection::TooManyTokens {
                observed: token_count,
            });
        }
    }
    Ok(())
}

fn validate_ranking_context(
    query: &CompletionQuery,
    context: LocalRankingContext<'_>,
) -> Result<(), PresentationRejection> {
    if let Some(prior) = context.prior_skeleton {
        if prior.len() > MAX_RANKING_SKELETON_BYTES {
            return Err(PresentationRejection::PriorSkeletonTooLarge {
                bytes: prior.len(),
                limit: MAX_RANKING_SKELETON_BYTES,
            });
        }
        match validate_canonical_skeleton(prior) {
            Ok(()) => {}
            Err(SkeletonRejection::Invalid) => {
                return Err(PresentationRejection::InvalidPriorSkeleton);
            }
            Err(SkeletonRejection::TooManyTokens { observed }) => {
                return Err(PresentationRejection::TooManyPriorSkeletonTokens {
                    observed,
                    limit: MAX_RANKING_SKELETON_TOKENS,
                });
            }
        }
    }

    validate_workspace_context(query, context.workspace)
}

fn validate_workspace_context(
    query: &CompletionQuery,
    workspace: &WorkspaceContext,
) -> Result<(), PresentationRejection> {
    let cwd_bytes = path_bytes(&workspace.cwd);
    if cwd_bytes > MAX_WORKSPACE_PATH_BYTES {
        return Err(PresentationRejection::WorkspaceCwdTooLarge {
            bytes: cwd_bytes,
            limit: MAX_WORKSPACE_PATH_BYTES,
        });
    }
    if !workspace.cwd.is_absolute() {
        return Err(PresentationRejection::WorkspaceCwdNotAbsolute);
    }
    if workspace.cwd != query.cwd {
        return Err(PresentationRejection::WorkspaceCwdMismatch);
    }
    if workspace.signatures.len() > MAX_WORKSPACE_SIGNATURES {
        return Err(PresentationRejection::TooManyWorkspaceSignatures {
            observed: workspace.signatures.len(),
            limit: MAX_WORKSPACE_SIGNATURES,
        });
    }

    let mut kinds = BTreeSet::new();
    for (index, signature) in workspace.signatures.iter().enumerate() {
        if !kinds.insert(signature.kind) {
            return Err(PresentationRejection::DuplicateWorkspaceKind {
                index,
                kind: signature.kind,
            });
        }

        let root_bytes = path_bytes(&signature.root);
        if root_bytes > MAX_WORKSPACE_PATH_BYTES {
            return Err(PresentationRejection::WorkspaceRootTooLarge {
                index,
                bytes: root_bytes,
                limit: MAX_WORKSPACE_PATH_BYTES,
            });
        }
        if !signature.root.is_absolute() {
            return Err(PresentationRejection::WorkspaceRootNotAbsolute { index });
        }

        let marker_bytes = path_bytes(&signature.marker);
        if marker_bytes > MAX_WORKSPACE_PATH_BYTES {
            return Err(PresentationRejection::WorkspaceMarkerTooLarge {
                index,
                bytes: marker_bytes,
                limit: MAX_WORKSPACE_PATH_BYTES,
            });
        }
        if !signature.marker.is_absolute() {
            return Err(PresentationRejection::WorkspaceMarkerNotAbsolute { index });
        }
    }

    Ok(())
}

fn validate_batch(batch: ProviderBatch) -> Result<StoredBatch, BatchRejection> {
    let ProviderBatch {
        suggestions, error, ..
    } = batch;
    if error.is_some() && !suggestions.is_empty() {
        return Err(BatchRejection::ConflictingSuccessAndFailure);
    }
    if suggestions.len() > MAX_BATCH_CANDIDATES {
        return Err(BatchRejection::TooManyCandidates {
            observed: suggestions.len(),
            limit: MAX_BATCH_CANDIDATES,
        });
    }

    let error = match error {
        Some(error) => {
            if error.len() > MAX_PROVIDER_ERROR_BYTES {
                return Err(BatchRejection::ErrorTooLarge {
                    bytes: error.len(),
                    limit: MAX_PROVIDER_ERROR_BYTES,
                });
            }
            if error.chars().any(char::is_control) {
                return Err(BatchRejection::UnsafeErrorText);
            }
            Some(error.into_boxed_str())
        }
        None => None,
    };

    let mut retained_bytes = error.as_ref().map_or(0, |value| value.len());
    let mut normalized = Vec::with_capacity(suggestions.len());
    for (index, candidate) in suggestions.into_iter().enumerate() {
        let bytes = candidate_bytes(&candidate);
        if bytes > MAX_CANDIDATE_BYTES {
            return Err(BatchRejection::CandidateTooLarge {
                index,
                bytes,
                limit: MAX_CANDIDATE_BYTES,
            });
        }
        retained_bytes = retained_bytes.saturating_add(bytes);
        if retained_bytes > MAX_BATCH_BYTES {
            return Err(BatchRejection::BatchTooLarge {
                bytes: retained_bytes,
                limit: MAX_BATCH_BYTES,
            });
        }
        normalized.push(normalize_candidate(candidate));
    }

    Ok(StoredBatch {
        suggestions: normalized.into_boxed_slice(),
        error,
        retained_bytes,
    })
}

fn validate_and_normalize_ranked_candidates(
    merged: &[Suggestion],
    ranked: Vec<Suggestion>,
) -> Result<Vec<Suggestion>, PresentationRejection> {
    if ranked.len() > MAX_CUMULATIVE_CANDIDATES {
        return Err(PresentationRejection::TooManyCandidates {
            observed: ranked.len(),
            limit: MAX_CUMULATIVE_CANDIDATES,
        });
    }

    let mut cumulative_bytes = 0usize;
    for (index, candidate) in ranked.iter().enumerate() {
        let bytes = candidate_bytes(candidate);
        if bytes > MAX_CANDIDATE_BYTES {
            return Err(PresentationRejection::CandidateTooLarge {
                index,
                bytes,
                limit: MAX_CANDIDATE_BYTES,
            });
        }
        cumulative_bytes = cumulative_bytes.saturating_add(bytes);
        if cumulative_bytes > MAX_CUMULATIVE_BYTES {
            return Err(PresentationRejection::CumulativeBytesTooLarge {
                bytes: cumulative_bytes,
                limit: MAX_CUMULATIVE_BYTES,
            });
        }
    }
    if !is_exact_permutation(merged, &ranked) {
        return Err(PresentationRejection::CandidateSetMismatch);
    }
    Ok(ranked
        .into_iter()
        .map(normalize_candidate)
        .collect::<Vec<_>>()
        .into_boxed_slice()
        .into_vec())
}

fn is_exact_permutation(expected: &[Suggestion], actual: &[Suggestion]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }

    let mut expected = expected.iter().map(CandidateKey::new).collect::<Vec<_>>();
    let mut actual = actual.iter().map(CandidateKey::new).collect::<Vec<_>>();
    expected.sort_unstable();
    actual.sort_unstable();
    expected == actual
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct CandidateKey<'a> {
    range_start: usize,
    range_end: usize,
    replacement: &'a str,
    display: &'a str,
    description: &'a str,
    icon: &'a str,
    source: SuggestionSource,
    sources: &'a BTreeSet<SuggestionSource>,
    static_priority_bits: u64,
    confidence_bits: u64,
    insertion: u8,
    identity: &'a str,
}

impl<'a> CandidateKey<'a> {
    fn new(candidate: &'a Suggestion) -> Self {
        Self {
            range_start: candidate.edit().range.start,
            range_end: candidate.edit().range.end,
            replacement: &candidate.edit().replacement,
            display: candidate.display(),
            description: candidate.description(),
            icon: candidate.icon(),
            source: candidate.source(),
            sources: candidate.sources(),
            static_priority_bits: candidate.static_priority().to_bits(),
            confidence_bits: candidate.confidence().to_bits(),
            insertion: insertion_key(candidate.insertion()),
            identity: candidate.identity(),
        }
    }
}

const fn insertion_key(insertion: InsertionBehavior) -> u8 {
    match insertion {
        InsertionBehavior::Exact => 0,
        InsertionBehavior::AppendSpace => 1,
        InsertionBehavior::Directory => 2,
    }
}

fn normalize_candidate(mut candidate: Suggestion) -> Suggestion {
    candidate.edit.replacement = compact_string(candidate.edit.replacement);
    candidate.display = compact_string(candidate.display);
    candidate.description = compact_string(candidate.description);
    candidate.icon = compact_string(candidate.icon);
    candidate.identity = compact_string(candidate.identity);
    candidate
}

fn candidate_bytes(candidate: &Suggestion) -> usize {
    candidate
        .edit()
        .replacement
        .len()
        .saturating_add(candidate.display().len())
        .saturating_add(candidate.description().len())
        .saturating_add(candidate.icon().len())
        .saturating_add(candidate.identity().len())
}

fn merged_for(active: &ActiveQuery) -> Vec<Suggestion> {
    merge_suggestions(
        &active.query,
        active
            .batches
            .values()
            .map(|batch| batch.suggestions.to_vec()),
    )
}

fn compact_string(value: String) -> String {
    value.into_boxed_str().into_string()
}

fn compact_path(value: PathBuf) -> PathBuf {
    value.into_boxed_path().into_path_buf()
}

fn path_bytes(value: &Path) -> usize {
    value.as_os_str().as_encoded_bytes().len()
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use crate::completion::TextEdit;
    use crate::learning::LearningState;
    use crate::providers::{WorkspaceContext, WorkspaceKind, WorkspaceSignature};

    use super::*;

    const PROVIDERS: [&str; 3] = ["alias", "spec", "system"];

    fn coordinator(max_suggestions: usize) -> CompletionCoordinator {
        CompletionCoordinator::new(PROVIDERS, max_suggestions).unwrap()
    }

    #[test]
    fn live_result_limit_cancels_old_authority_and_validates_atomically() {
        let mut coordinator = coordinator(100);
        let generation = start(&mut coordinator, "git").query().generation;
        coordinator.reconfigure_ui_limit(5).unwrap();
        assert!(coordinator.active_query().is_none());
        assert!(matches!(
            coordinator.merged_candidates(generation),
            Err(AuthorityRejection::Cancelled { .. })
        ));
        assert!(matches!(
            coordinator.reconfigure_ui_limit(0),
            Err(RegistrationError::InvalidUiLimit { .. })
        ));
    }

    fn start(coordinator: &mut CompletionCoordinator, line: &str) -> QueryWork {
        coordinator
            .start_query(line, line.len(), Path::new("/tmp/Greendale"))
            .unwrap()
    }

    fn candidate(value: &str, identity: &str) -> Suggestion {
        candidate_with_range(value, identity, 0..1)
    }

    fn candidate_with_range(value: &str, identity: &str, range: Range<usize>) -> Suggestion {
        Suggestion::new(
            TextEdit {
                range,
                replacement: value.to_owned(),
            },
            value,
            format!("{value} from Greendale"),
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            identity,
        )
    }

    fn accepted(outcome: BatchOutcome) -> BatchAcceptance {
        let BatchOutcome::Accepted(acceptance) = outcome else {
            panic!("expected accepted batch, got {outcome:?}");
        };
        acceptance
    }

    fn rejected(outcome: BatchOutcome) -> BatchRejection {
        let BatchOutcome::Rejected(rejection) = outcome else {
            panic!("expected rejected batch, got {outcome:?}");
        };
        rejection
    }

    fn ranking_context<'a>(
        workspace: &'a WorkspaceContext,
        learning: &'a LearningState,
    ) -> LocalRankingContext<'a> {
        LocalRankingContext {
            workspace,
            learning,
            cwd: &workspace.cwd,
            now: 4_000_000,
            prior_skeleton: None,
        }
    }

    fn empty_workspace() -> WorkspaceContext {
        WorkspaceContext {
            cwd: PathBuf::from("/tmp/Greendale"),
            signatures: Vec::new(),
        }
    }

    fn apply_local(
        coordinator: &mut CompletionCoordinator,
        generation: u64,
    ) -> PresentationOutcome {
        let workspace = empty_workspace();
        let learning = LearningState::default();
        coordinator.rank_and_apply(generation, ranking_context(&workspace, &learning), |item| {
            LocalRankingMetadata::new(item.identity(), 0.5)
        })
    }

    #[test]
    fn registration_rejects_duplicate_invalid_excessive_names_and_ui_limits() {
        assert!(matches!(
            CompletionCoordinator::new(["spec", "spec"], 10),
            Err(RegistrationError::DuplicateProvider {
                index: 1,
                provider_bytes: 4
            })
        ));
        assert!(matches!(
            CompletionCoordinator::new([" spec"], 10),
            Err(RegistrationError::InvalidProviderName { index: 0 })
        ));
        let long_name = "x".repeat(MAX_PROVIDER_NAME_BYTES + 1).leak();
        assert!(matches!(
            CompletionCoordinator::new([long_name as &'static str], 10),
            Err(RegistrationError::ProviderNameTooLong { index: 0, .. })
        ));
        assert!(matches!(
            CompletionCoordinator::new(["spec"], 0),
            Err(RegistrationError::InvalidUiLimit { value: 0, .. })
        ));
    }

    #[test]
    fn cancellation_authority_is_exclusive_and_observers_share_state() {
        let (source, first) = CancellationSource::pair();
        let second = first.clone();
        assert!(!first.is_cancelled());
        assert!(!second.is_cancelled());
        source.cancel();
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());

        let observer = {
            let (_source, observer) = CancellationSource::pair();
            observer
        };
        assert!(
            observer.is_cancelled(),
            "dropping authority must cancel work"
        );
    }

    #[test]
    fn coordinator_is_intentionally_not_clone() {
        trait AmbiguousIfClone<A> {
            fn assert_not_clone() {}
        }
        impl<T: ?Sized> AmbiguousIfClone<()> for T {}
        impl<T: Clone> AmbiguousIfClone<u8> for T {}

        let _ = <CompletionCoordinator as AmbiguousIfClone<_>>::assert_not_clone;
    }

    #[test]
    fn cloned_work_shares_one_query_allocation_and_observer_state() {
        let mut coordinator = coordinator(10);
        let first = start(&mut coordinator, "git status");
        let second = first.clone();
        assert!(std::ptr::eq(first.query(), second.query()));
        let _ = coordinator.cancel_active_query();
        assert!(first.cancellation().is_cancelled());
        assert!(second.cancellation().is_cancelled());
    }

    #[test]
    fn query_bounds_and_capacities_are_normalized_before_retention() {
        let mut coordinator = coordinator(10);
        let mut line = String::with_capacity(MAX_QUERY_LINE_BYTES);
        line.push_str("git status");
        let mut cwd = PathBuf::with_capacity(MAX_QUERY_CWD_BYTES);
        cwd.push("/tmp/Greendale");
        let work = coordinator.start_query(line, 10, cwd).unwrap();
        assert_eq!(work.query().line.capacity(), work.query().line.len());
        assert_eq!(work.query().cwd.capacity(), path_bytes(&work.query().cwd));

        assert!(matches!(
            coordinator.start_query("x".repeat(MAX_QUERY_LINE_BYTES + 1), 0, Path::new("/tmp")),
            Err(QueryStartError::LineTooLarge { .. })
        ));
        assert!(work.cancellation().is_cancelled());

        assert!(matches!(
            coordinator.start_query("g", 1, PathBuf::from("x".repeat(MAX_QUERY_CWD_BYTES + 1))),
            Err(QueryStartError::CwdTooLarge { .. })
        ));
        assert!(matches!(
            coordinator.start_query("g", 1, Path::new("relative/workspace")),
            Err(QueryStartError::CwdNotAbsolute)
        ));
        assert!(matches!(
            coordinator.start_query("g", 1, Path::new("")),
            Err(QueryStartError::CwdNotAbsolute)
        ));
        assert!(matches!(
            coordinator.start_query("☃", 1, Path::new("/tmp")),
            Err(QueryStartError::InvalidCursor { .. })
        ));
    }

    #[test]
    fn retained_batch_strings_and_vectors_have_no_spare_capacity() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        let mut replacement = String::with_capacity(8_192);
        replacement.push_str("git");
        let mut identity = String::with_capacity(8_192);
        identity.push_str("git-identity");
        let suggestion = Suggestion::new(
            TextEdit {
                range: 0..1,
                replacement,
            },
            "git",
            "",
            "command",
            SuggestionSource::Spec,
            InsertionBehavior::Exact,
            identity,
        );
        let mut suggestions = Vec::with_capacity(MAX_BATCH_CANDIDATES);
        suggestions.push(suggestion);
        accepted(coordinator.accept_batch(ProviderBatch::success("spec", generation, suggestions)));

        let stored = &coordinator.active.as_ref().unwrap().batches["spec"];
        assert_eq!(stored.suggestions.len(), 1);
        let stored = &stored.suggestions[0];
        assert_eq!(
            stored.edit().replacement.capacity(),
            stored.edit().replacement.len()
        );
        assert_eq!(stored.identity.capacity(), stored.identity.len());
    }

    #[test]
    fn starting_query_cancels_and_clears_every_part_of_prior_work() {
        let mut coordinator = coordinator(10);
        let first = start(&mut coordinator, "g");
        let generation = first.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { displayed: 1, .. }
        ));

        let second = start(&mut coordinator, "c");
        assert!(first.cancellation().is_cancelled());
        assert_eq!(second.query().generation, generation + 1);
        assert!(coordinator.selection().selected().is_none());
        assert!(coordinator.selection().candidates().is_empty());
        assert!(
            coordinator
                .provider_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.phase() == ProviderPhase::Pending)
        );
    }

    #[test]
    fn stale_cancelled_and_rejected_updates_are_atomic() {
        let mut coordinator = coordinator(10);
        let first = start(&mut coordinator, "g");
        let first_generation = first.query().generation;
        let second = start(&mut coordinator, "c");
        let generation = second.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("cargo", "cargo")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { .. }
        ));
        let selected = coordinator.selection().selected().cloned();

        assert!(matches!(
            rejected(coordinator.accept_batch(ProviderBatch::success(
                "spec",
                first_generation,
                vec![candidate("git", "stale")],
            ))),
            BatchRejection::Authority(AuthorityRejection::GenerationMismatch { .. })
        ));
        assert!(matches!(
            rejected(coordinator.accept_batch(ProviderBatch::success(
                "missing",
                generation,
                vec![candidate("curl", "unknown")],
            ))),
            BatchRejection::UnknownProvider { .. }
        ));
        assert_eq!(coordinator.selection().selected(), selected.as_ref());

        let workspace = empty_workspace();
        let learning = LearningState::default();
        assert!(matches!(
            coordinator.rank_and_apply(
                first_generation,
                ranking_context(&workspace, &learning),
                |_| panic!("stale work must not invoke ranking metadata"),
            ),
            PresentationOutcome::Rejected(PresentationRejection::Authority(
                AuthorityRejection::GenerationMismatch { .. }
            ))
        ));
        assert_eq!(coordinator.selection().selected(), selected.as_ref());

        assert_eq!(
            coordinator.cancel_active_query(),
            CancellationOutcome::Cancelled { generation }
        );
        assert!(second.cancellation().is_cancelled());
        assert!(matches!(
            rejected(coordinator.accept_batch(ProviderBatch::success(
                "spec",
                generation,
                vec![candidate("curl", "cancelled")],
            ))),
            BatchRejection::Authority(AuthorityRejection::Cancelled { .. })
        ));
        assert!(matches!(
            coordinator.rank_and_apply(
                generation,
                ranking_context(&workspace, &learning),
                |_| panic!("cancelled work must not invoke ranking metadata"),
            ),
            PresentationOutcome::Rejected(PresentationRejection::Authority(
                AuthorityRejection::Cancelled { .. }
            ))
        ));
        assert!(coordinator.selection().selected().is_none());
    }

    #[test]
    fn provider_replacement_and_failure_are_isolated() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "system",
            generation,
            vec![candidate("gawk", "gawk")],
        )));

        let replacement = accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("go", "go")],
        )));
        assert!(replacement.replaced_previous());
        assert_eq!(coordinator.merged_candidates(generation).unwrap().len(), 2);

        let failure = accepted(coordinator.accept_batch(ProviderBatch::failure(
            "spec",
            generation,
            "Git metadata unavailable",
        )));
        assert_eq!(failure.kind(), AcceptedBatchKind::Failure);
        let diagnostics = coordinator.provider_diagnostics();
        let spec = diagnostics
            .iter()
            .find(|item| item.provider() == "spec")
            .unwrap();
        assert_eq!(spec.phase(), ProviderPhase::Failed);
        assert_eq!(spec.error(), Some("Git metadata unavailable"));
        assert_eq!(coordinator.merged_candidates(generation).unwrap().len(), 1);
    }

    #[test]
    fn full_set_local_ranking_precedes_the_five_hundred_item_ui_ceiling() {
        let mut coordinator = CompletionCoordinator::new(["first", "second"], 500).unwrap();
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        for (provider, start_index) in [("first", 0), ("second", 300)] {
            let candidates = (start_index..start_index + 300)
                .map(|index| {
                    candidate(
                        &format!("command-{index:03}"),
                        &format!("identity-{index:03}"),
                    )
                })
                .collect();
            accepted(
                coordinator.accept_batch(ProviderBatch::success(provider, generation, candidates)),
            );
        }

        let workspace = empty_workspace();
        let learning = LearningState::default();
        let outcome = coordinator.rank_and_apply(
            generation,
            ranking_context(&workspace, &learning),
            |item| {
                LocalRankingMetadata::new(
                    item.identity(),
                    f64::from(item.identity() == "identity-599"),
                )
            },
        );
        assert_eq!(
            outcome,
            PresentationOutcome::Applied {
                available: 600,
                displayed: 500,
            }
        );
        assert_eq!(
            coordinator.selection().selected().unwrap().identity(),
            "identity-599"
        );
        assert_eq!(coordinator.selection().candidates().len(), 500);
    }

    #[test]
    fn nan_fields_do_not_break_exact_linearithmic_permutation_validation() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        let mut nan = candidate("git", "nan");
        nan.static_priority = f64::NAN;
        nan.confidence = f64::from_bits(0x7ff8_0000_0000_0001);
        accepted(coordinator.accept_batch(ProviderBatch::success("spec", generation, vec![nan])));

        let workspace = empty_workspace();
        let learning = LearningState::default();
        assert!(matches!(
            coordinator.rank_and_apply(generation, ranking_context(&workspace, &learning), |_| {
                LocalRankingMetadata::new("git", f64::NAN)
            },),
            PresentationOutcome::Applied { displayed: 1, .. }
        ));
    }

    #[test]
    fn exact_ranked_validation_counts_duplicate_candidates() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git"), candidate("go", "go")],
        )));
        let mut ranked = coordinator.merged_candidates(generation).unwrap();
        ranked[1] = ranked[0].clone();
        assert_eq!(
            coordinator.apply_ranked(generation, ranked),
            PresentationOutcome::Rejected(PresentationRejection::CandidateSetMismatch)
        );
        assert!(coordinator.selection().selected().is_none());
    }

    #[test]
    fn navigation_freezes_visible_order_against_late_top_ranked_results() {
        let mut coordinator = CompletionCoordinator::new(["spec"], 2).unwrap();
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("alpha", "alpha"), candidate("beta", "beta")],
        )));

        let mut initial = coordinator.merged_candidates(generation).unwrap();
        initial.sort_by(|left, right| left.identity().cmp(right.identity()));
        assert!(matches!(
            coordinator.apply_ranked(generation, initial),
            PresentationOutcome::Applied { .. }
        ));
        coordinator.select_next();
        assert_eq!(
            coordinator.selection().selected().unwrap().identity(),
            "beta"
        );

        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![
                candidate("aardvark", "aardvark"),
                candidate("alpha", "alpha"),
                candidate("beta", "beta"),
            ],
        )));
        let mut update = coordinator.merged_candidates(generation).unwrap();
        update.sort_by(|left, right| left.identity().cmp(right.identity()));
        assert!(matches!(
            coordinator.apply_ranked(generation, update),
            PresentationOutcome::Applied { displayed: 2, .. }
        ));
        assert_eq!(
            coordinator
                .selection()
                .candidates()
                .iter()
                .map(Suggestion::identity)
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(
            coordinator.selection().selected().unwrap().identity(),
            "beta"
        );
    }

    #[test]
    fn ranking_metadata_rejection_leaves_selection_unchanged() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { .. }
        ));
        let selected = coordinator.selection().selected().cloned();

        let workspace = empty_workspace();
        let learning = LearningState::default();
        assert!(matches!(
            coordinator.rank_and_apply(generation, ranking_context(&workspace, &learning), |_| {
                LocalRankingMetadata::new("x".repeat(MAX_RANKING_SKELETON_BYTES + 1), 1.0)
            },),
            PresentationOutcome::Rejected(PresentationRejection::RankingSkeletonTooLarge { .. })
        ));
        assert_eq!(coordinator.selection().selected(), selected.as_ref());
    }

    #[test]
    fn canonical_skeleton_bounds_are_validated_before_ranking() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { .. }
        ));
        let selected = coordinator.selection().selected().cloned();
        let workspace = empty_workspace();
        let learning = LearningState::default();

        for (skeleton, rejection) in [
            (
                "git  status".to_owned(),
                PresentationRejection::InvalidRankingSkeleton { index: 0 },
            ),
            (
                (0..=MAX_RANKING_SKELETON_TOKENS)
                    .map(|_| "token")
                    .collect::<Vec<_>>()
                    .join(" "),
                PresentationRejection::TooManyRankingSkeletonTokens {
                    index: 0,
                    observed: MAX_RANKING_SKELETON_TOKENS + 1,
                    limit: MAX_RANKING_SKELETON_TOKENS,
                },
            ),
        ] {
            assert_eq!(
                coordinator.rank_and_apply(
                    generation,
                    ranking_context(&workspace, &learning),
                    |_| LocalRankingMetadata::new(&skeleton, 1.0),
                ),
                PresentationOutcome::Rejected(rejection)
            );
            assert_eq!(coordinator.selection().selected(), selected.as_ref());
        }

        let oversized = "x".repeat(MAX_RANKING_SKELETON_BYTES + 1);
        assert!(matches!(
            coordinator.rank_and_apply(generation, ranking_context(&workspace, &learning), |_| {
                LocalRankingMetadata::new(&oversized, 1.0)
            },),
            PresentationOutcome::Rejected(PresentationRejection::RankingSkeletonTooLarge { .. })
        ));
        assert_eq!(coordinator.selection().selected(), selected.as_ref());
    }

    #[test]
    fn prior_skeleton_is_bounded_and_canonical_even_without_candidates() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        let workspace = empty_workspace();
        let learning = LearningState::default();
        let too_many_tokens = (0..=MAX_RANKING_SKELETON_TOKENS)
            .map(|_| "token")
            .collect::<Vec<_>>()
            .join(" ");

        for (prior, rejection) in [
            (
                "git  status".to_owned(),
                PresentationRejection::InvalidPriorSkeleton,
            ),
            (
                too_many_tokens,
                PresentationRejection::TooManyPriorSkeletonTokens {
                    observed: MAX_RANKING_SKELETON_TOKENS + 1,
                    limit: MAX_RANKING_SKELETON_TOKENS,
                },
            ),
            (
                "x".repeat(MAX_RANKING_SKELETON_BYTES + 1),
                PresentationRejection::PriorSkeletonTooLarge {
                    bytes: MAX_RANKING_SKELETON_BYTES + 1,
                    limit: MAX_RANKING_SKELETON_BYTES,
                },
            ),
        ] {
            let mut context = ranking_context(&workspace, &learning);
            context.prior_skeleton = Some(&prior);
            assert_eq!(
                coordinator.rank_and_apply(generation, context, |_| {
                    panic!("invalid prior context must be rejected before metadata")
                }),
                PresentationOutcome::Rejected(rejection)
            );
            assert!(coordinator.selection().candidates().is_empty());
        }
    }

    #[test]
    fn ranking_workspace_context_must_match_query_and_remain_bounded() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { .. }
        ));
        let selected = coordinator.selection().selected().cloned();
        let learning = LearningState::default();

        let contexts = [
            (
                WorkspaceContext {
                    cwd: PathBuf::from("relative"),
                    signatures: Vec::new(),
                },
                PresentationRejection::WorkspaceCwdNotAbsolute,
            ),
            (
                WorkspaceContext {
                    cwd: PathBuf::from("/tmp/CityCollege"),
                    signatures: Vec::new(),
                },
                PresentationRejection::WorkspaceCwdMismatch,
            ),
            (
                WorkspaceContext {
                    cwd: PathBuf::from("/").join("x".repeat(MAX_WORKSPACE_PATH_BYTES + 1)),
                    signatures: Vec::new(),
                },
                PresentationRejection::WorkspaceCwdTooLarge {
                    bytes: MAX_WORKSPACE_PATH_BYTES + 2,
                    limit: MAX_WORKSPACE_PATH_BYTES,
                },
            ),
        ];
        for (workspace, rejection) in contexts {
            assert_eq!(
                coordinator.rank_and_apply(
                    generation,
                    ranking_context(&workspace, &learning),
                    |_| LocalRankingMetadata::new("git", 1.0),
                ),
                PresentationOutcome::Rejected(rejection)
            );
            assert_eq!(coordinator.selection().selected(), selected.as_ref());
        }
    }

    #[test]
    fn ranking_workspace_signatures_are_unique_and_count_bounded() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        let learning = LearningState::default();
        let cwd = PathBuf::from("/tmp/Greendale");

        let duplicate = WorkspaceContext {
            cwd: cwd.clone(),
            signatures: vec![
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: cwd.clone(),
                    marker: cwd.join(".git"),
                },
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: cwd.clone(),
                    marker: cwd.join("nested/.git"),
                },
            ],
        };
        assert_eq!(
            coordinator.rank_and_apply(generation, ranking_context(&duplicate, &learning), |_| {
                LocalRankingMetadata::new("git", 1.0)
            },),
            PresentationOutcome::Rejected(PresentationRejection::DuplicateWorkspaceKind {
                index: 1,
                kind: WorkspaceKind::Git,
            })
        );

        let excessive = WorkspaceContext {
            cwd: cwd.clone(),
            signatures: (0..=MAX_WORKSPACE_SIGNATURES)
                .map(|index| WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: cwd.clone(),
                    marker: cwd.join(format!("marker-{index}")),
                })
                .collect(),
        };
        assert_eq!(
            coordinator.rank_and_apply(generation, ranking_context(&excessive, &learning), |_| {
                LocalRankingMetadata::new("git", 1.0)
            },),
            PresentationOutcome::Rejected(PresentationRejection::TooManyWorkspaceSignatures {
                observed: MAX_WORKSPACE_SIGNATURES + 1,
                limit: MAX_WORKSPACE_SIGNATURES,
            })
        );
        assert!(coordinator.selection().candidates().is_empty());
    }

    #[test]
    fn ranking_workspace_signature_paths_are_absolute_and_bounded() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        let learning = LearningState::default();
        let cwd = PathBuf::from("/tmp/Greendale");

        for (signature, rejection) in [
            (
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: PathBuf::from("relative"),
                    marker: cwd.join(".git"),
                },
                PresentationRejection::WorkspaceRootNotAbsolute { index: 0 },
            ),
            (
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: PathBuf::from("/").join("x".repeat(MAX_WORKSPACE_PATH_BYTES + 1)),
                    marker: cwd.join(".git"),
                },
                PresentationRejection::WorkspaceRootTooLarge {
                    index: 0,
                    bytes: MAX_WORKSPACE_PATH_BYTES + 2,
                    limit: MAX_WORKSPACE_PATH_BYTES,
                },
            ),
            (
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: cwd.clone(),
                    marker: PathBuf::from("relative"),
                },
                PresentationRejection::WorkspaceMarkerNotAbsolute { index: 0 },
            ),
            (
                WorkspaceSignature {
                    kind: WorkspaceKind::Git,
                    root: cwd.clone(),
                    marker: PathBuf::from("/").join("x".repeat(MAX_WORKSPACE_PATH_BYTES + 1)),
                },
                PresentationRejection::WorkspaceMarkerTooLarge {
                    index: 0,
                    bytes: MAX_WORKSPACE_PATH_BYTES + 2,
                    limit: MAX_WORKSPACE_PATH_BYTES,
                },
            ),
        ] {
            let workspace = WorkspaceContext {
                cwd: cwd.clone(),
                signatures: vec![signature],
            };
            assert_eq!(
                coordinator.rank_and_apply(
                    generation,
                    ranking_context(&workspace, &learning),
                    |_| LocalRankingMetadata::new("git", 1.0),
                ),
                PresentationOutcome::Rejected(rejection)
            );
        }
        assert!(coordinator.selection().candidates().is_empty());
    }

    #[test]
    fn aggregate_ranked_bytes_are_rejected_without_selection_changes() {
        let mut coordinator = coordinator(10);
        let work = start(&mut coordinator, "g");
        let generation = work.query().generation;
        accepted(coordinator.accept_batch(ProviderBatch::success(
            "spec",
            generation,
            vec![candidate("git", "git")],
        )));
        assert!(matches!(
            apply_local(&mut coordinator, generation),
            PresentationOutcome::Applied { .. }
        ));
        let selected = coordinator.selection().selected().cloned();

        let oversized = (0..=MAX_CUMULATIVE_BYTES / MAX_CANDIDATE_BYTES)
            .map(|_| {
                Suggestion::new(
                    TextEdit {
                        range: 0..1,
                        replacement: "x".repeat(MAX_CANDIDATE_BYTES - 1),
                    },
                    "x",
                    "",
                    "",
                    SuggestionSource::Spec,
                    InsertionBehavior::Exact,
                    "",
                )
            })
            .collect();
        assert!(matches!(
            coordinator.apply_ranked(generation, oversized),
            PresentationOutcome::Rejected(PresentationRejection::CumulativeBytesTooLarge { .. })
        ));
        assert_eq!(coordinator.selection().selected(), selected.as_ref());
    }

    #[test]
    fn debug_output_redacts_query_candidates_paths_and_errors() {
        let mut coordinator = CompletionCoordinator::new(["classified-provider"], 10).unwrap();
        let work = coordinator
            .start_query(
                "secret-command --token hunter2",
                30,
                Path::new("/private/secret-workspace"),
            )
            .unwrap();
        let generation = work.query().generation;
        let acceptance = accepted(coordinator.accept_batch(ProviderBatch::failure(
            "classified-provider",
            generation,
            "secret provider failure",
        )));

        let work_debug = format!("{work:?}");
        let coordinator_debug = format!("{coordinator:?}");
        let diagnostics_debug = format!("{:?}", coordinator.provider_diagnostics());
        let acceptance_debug = format!("{acceptance:?}");
        for secret in [
            "secret-command",
            "hunter2",
            "secret-workspace",
            "classified-provider",
            "secret provider failure",
        ] {
            assert!(!work_debug.contains(secret));
            assert!(!coordinator_debug.contains(secret));
            assert!(!diagnostics_debug.contains(secret));
            assert!(!acceptance_debug.contains(secret));
        }

        let unknown = rejected(coordinator.accept_batch(ProviderBatch::success(
            "secret-unknown-provider-hunter2",
            generation,
            Vec::new(),
        )));
        let unknown_debug = format!("{unknown:?} {unknown}");
        assert!(!unknown_debug.contains("secret-unknown-provider-hunter2"));
        assert!(matches!(
            unknown,
            BatchRejection::UnknownProvider { provider_bytes: 31 }
        ));

        let duplicate = CompletionCoordinator::new(
            ["secret-duplicate-provider", "secret-duplicate-provider"],
            10,
        )
        .unwrap_err();
        assert!(!format!("{duplicate:?} {duplicate}").contains("secret-duplicate-provider"));
    }

    #[test]
    fn generation_exhaustion_cancels_max_generation_without_wrapping() {
        let mut coordinator =
            CompletionCoordinator::with_next_generation(["spec"], 10, Some(u64::MAX)).unwrap();
        let max = start(&mut coordinator, "g");
        assert_eq!(max.query().generation, u64::MAX);
        assert!(matches!(
            coordinator.start_query("c", 1, Path::new("/tmp")),
            Err(QueryStartError::GenerationExhausted)
        ));
        assert!(max.cancellation().is_cancelled());
        assert!(coordinator.active_query().is_none());
        assert!(matches!(
            rejected(coordinator.accept_batch(ProviderBatch::success(
                "spec",
                u64::MAX,
                vec![candidate("git", "late-max")],
            ))),
            BatchRejection::Authority(AuthorityRejection::Cancelled {
                generation: u64::MAX
            })
        ));
    }
}
