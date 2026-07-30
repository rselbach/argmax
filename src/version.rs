//! Strict release-version parsing and automatic-update eligibility policy.
//!
//! The network updater can hand remote release tags to this module without
//! granting them authority to trigger an update. Malformed, ineligible, stale,
//! and development-build comparisons remain explicit outcomes.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

use crate::config::UpdateChannel;

/// Semantic version embedded in this executable.
///
/// Release builds may provide `ARGMAX_BUILD_VERSION` at compile time so a
/// nightly artifact is distinguishable from the package's stable version.
/// Ordinary local builds use the Cargo package version.
pub const RUNNING_VERSION: &str = match option_env!("ARGMAX_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Returns the validated semantic version embedded in this executable.
///
/// # Errors
///
/// Returns [`VersionError`] when a release build supplied malformed metadata.
pub fn running_version() -> Result<SemanticVersion, VersionError> {
    SemanticVersion::parse(RUNNING_VERSION)
}

/// Maximum accepted byte length for a local version or remote release tag.
pub const MAX_VERSION_BYTES: usize = 256;
/// Maximum prerelease identifiers retained for one version.
pub const MAX_PRERELEASE_IDENTIFIERS: usize = 64;

/// One validated semantic version.
#[derive(Clone)]
pub struct SemanticVersion {
    text: Box<str>,
    major: Box<str>,
    minor: Box<str>,
    patch: Box<str>,
    prerelease: Box<[PrereleaseIdentifier]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PrereleaseIdentifier {
    Numeric(Box<str>),
    Alphanumeric(Box<str>),
}

impl SemanticVersion {
    /// Parses a strict semantic version without a tag prefix.
    ///
    /// Build metadata is validated and retained in [`Self::as_str`], but does
    /// not affect precedence as required by Semantic Versioning.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError`] for empty, oversized, or malformed input.
    pub fn parse(value: &str) -> Result<Self, VersionError> {
        Self::parse_inner(value, false)
    }

    /// Parses a remote release tag, accepting one conventional lowercase `v`
    /// prefix in addition to a strict semantic version.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError`] when the remaining tag is not a valid semantic
    /// version.
    pub fn parse_release_tag(value: &str) -> Result<Self, VersionError> {
        Self::parse_inner(value, true)
    }

    /// Validated semantic-version text without a release-tag prefix.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Major version component.
    #[must_use]
    pub fn major(&self) -> &str {
        &self.major
    }

    /// Minor version component.
    #[must_use]
    pub fn minor(&self) -> &str {
        &self.minor
    }

    /// Patch version component.
    #[must_use]
    pub fn patch(&self) -> &str {
        &self.patch
    }

    /// Whether the version contains prerelease identifiers.
    #[must_use]
    pub const fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }

    /// Compares semantic-version precedence, ignoring build metadata.
    #[must_use]
    pub fn precedence_cmp(&self, other: &Self) -> Ordering {
        compare_numeric_text(&self.major, &other.major)
            .then_with(|| compare_numeric_text(&self.minor, &other.minor))
            .then_with(|| compare_numeric_text(&self.patch, &other.patch))
            .then_with(|| compare_prerelease(&self.prerelease, &other.prerelease))
    }

    /// Whether both values have identical semantic-version text, including
    /// prerelease and build metadata.
    #[must_use]
    pub fn has_same_identity(&self, other: &Self) -> bool {
        self.text == other.text
    }

    /// Whether both values have equal precedence after ignoring build metadata.
    #[must_use]
    pub fn has_same_precedence(&self, other: &Self) -> bool {
        self.precedence_cmp(other).is_eq()
    }

    fn parse_inner(value: &str, allow_tag_prefix: bool) -> Result<Self, VersionError> {
        if value.is_empty() {
            return Err(VersionError::Empty);
        }
        if value.len() > MAX_VERSION_BYTES {
            return Err(VersionError::TooLong {
                bytes: value.len(),
                limit: MAX_VERSION_BYTES,
            });
        }
        if value.trim() != value {
            return Err(VersionError::InvalidSyntax);
        }

        let semantic = if allow_tag_prefix {
            value.strip_prefix('v').unwrap_or(value)
        } else {
            value
        };
        if semantic.is_empty() || semantic.starts_with('v') {
            return Err(VersionError::InvalidSyntax);
        }

        let (without_build, build) = split_once_unique(semantic, '+')?;
        if let Some(build) = build {
            validate_dot_identifiers(build, false)?;
        }
        let (core, prerelease_text) = match without_build.split_once('-') {
            Some((core, prerelease)) if !core.is_empty() && !prerelease.is_empty() => {
                (core, Some(prerelease))
            }
            Some(_) => return Err(VersionError::InvalidSyntax),
            None => (without_build, None),
        };

        let mut core_parts = core.split('.');
        let major = parse_core_number(core_parts.next().unwrap_or_default())?;
        let minor = parse_core_number(core_parts.next().unwrap_or_default())?;
        let patch = parse_core_number(core_parts.next().unwrap_or_default())?;
        if core_parts.next().is_some() {
            return Err(VersionError::InvalidCore);
        }

        let prerelease = match prerelease_text {
            Some(prerelease) => parse_prerelease(prerelease)?,
            None => Box::default(),
        };

        Ok(Self {
            text: semantic.into(),
            major,
            minor,
            patch,
            prerelease,
        })
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl fmt::Debug for SemanticVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticVersion")
            .field("bytes", &self.text.len())
            .field("major_digits", &self.major.len())
            .field("minor_digits", &self.minor.len())
            .field("patch_digits", &self.patch.len())
            .field("prerelease_identifiers", &self.prerelease.len())
            .finish()
    }
}

/// Trusted release-feed classification for one remote artifact.
///
/// This is distinct from [`UpdateChannel`]: a configured nightly channel may
/// accept both stable promotions and nightly prereleases, while one artifact
/// has exactly one trusted classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseKind {
    /// A final release artifact.
    Stable,
    /// A nightly artifact whose version must contain prerelease identifiers.
    Nightly,
}

/// Trusted release-feed classification for one remote artifact.
///
/// The updater must derive this value from the selected, authenticated release
/// feed or verified release metadata. It must never infer it from untrusted tag
/// text or build metadata alone.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RemoteRelease<'a> {
    tag: &'a str,
    kind: ReleaseKind,
}

impl<'a> RemoteRelease<'a> {
    /// Associates one remote tag with its trusted artifact classification.
    #[must_use]
    pub const fn new(tag: &'a str, kind: ReleaseKind) -> Self {
        Self { tag, kind }
    }

    /// Unparsed remote release tag.
    #[must_use]
    pub const fn tag(self) -> &'a str {
        self.tag
    }

    /// Trusted artifact classification from release metadata.
    #[must_use]
    pub const fn kind(self) -> ReleaseKind {
        self.kind
    }
}

impl fmt::Debug for RemoteRelease<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteRelease")
            .field("kind", &self.kind)
            .field("tag_bytes", &self.tag.len())
            .finish()
    }
}

/// Safe result of comparing one running build with one remote release tag.
#[derive(Clone, Debug)]
pub enum AutomaticUpdateDecision {
    /// The remote version is eligible and newer than the running build.
    Available(SemanticVersion),
    /// The remote version is valid and eligible but not newer.
    Current,
    /// The selected stable channel excludes the trusted nightly artifact.
    ChannelMismatch,
    /// Release metadata and semantic-version form contradicted one another.
    ///
    /// Stable artifacts must be final versions. Nightly artifacts must use a
    /// monotonic prerelease component; build metadata alone cannot order them.
    InvalidRemoteMetadata,
    /// Empty and `dev` builds never produce an automatic notice.
    DevelopmentBuild,
    /// The running build is neither a development marker nor a semantic version.
    InvalidCurrentVersion(VersionError),
    /// The remote tag is malformed and therefore cannot trigger an update.
    InvalidRemoteVersion(VersionError),
}

/// Applies automatic-update version and channel policy to one remote release.
///
/// The comparison is side-effect free. In particular, a malformed remote value
/// is represented as a rejection and never as an available update. The caller
/// remains responsible for selecting the candidate from a channel-aware release
/// feed; this function validates one candidate rather than searching a feed. A
/// configured stable channel accepts only stable artifacts. A configured nightly
/// channel accepts stable final promotions and ordered nightly prereleases.
#[must_use]
pub fn decide_automatic_update(
    current: &str,
    remote_release: RemoteRelease<'_>,
    configured_channel: UpdateChannel,
) -> AutomaticUpdateDecision {
    if current.is_empty() || current.eq_ignore_ascii_case("dev") {
        return AutomaticUpdateDecision::DevelopmentBuild;
    }

    let current = match SemanticVersion::parse(current) {
        Ok(version) => version,
        Err(error) => return AutomaticUpdateDecision::InvalidCurrentVersion(error),
    };
    let remote = match SemanticVersion::parse_release_tag(remote_release.tag()) {
        Ok(version) => version,
        Err(error) => return AutomaticUpdateDecision::InvalidRemoteVersion(error),
    };

    let metadata_is_consistent = match remote_release.kind() {
        ReleaseKind::Stable => !remote.is_prerelease(),
        ReleaseKind::Nightly => remote.is_prerelease(),
    };
    if !metadata_is_consistent {
        return AutomaticUpdateDecision::InvalidRemoteMetadata;
    }
    if configured_channel == UpdateChannel::Stable && remote_release.kind() == ReleaseKind::Nightly
    {
        return AutomaticUpdateDecision::ChannelMismatch;
    }
    if remote.precedence_cmp(&current).is_gt() {
        AutomaticUpdateDecision::Available(remote)
    } else {
        AutomaticUpdateDecision::Current
    }
}

/// Why a version string was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionError {
    /// No version text was supplied.
    Empty,
    /// The version exceeded the defensive input bound.
    TooLong {
        /// Observed byte length.
        bytes: usize,
        /// Accepted byte limit.
        limit: usize,
    },
    /// Separators, whitespace, or a release-tag prefix were malformed.
    InvalidSyntax,
    /// Major, minor, and patch were not exactly three decimal components.
    InvalidCore,
    /// A numeric component used a forbidden leading zero.
    LeadingZero,
    /// A prerelease or build identifier was empty or contained an invalid byte.
    InvalidIdentifier,
    /// The prerelease identifier count exceeded the defensive bound.
    TooManyPrereleaseIdentifiers {
        /// Accepted identifier limit.
        limit: usize,
    },
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("version is empty"),
            Self::TooLong { bytes, limit } => {
                write!(formatter, "version is {bytes} bytes; limit is {limit}")
            }
            Self::InvalidSyntax => formatter.write_str("version syntax is invalid"),
            Self::InvalidCore => {
                formatter.write_str("version must contain decimal major, minor, and patch")
            }
            Self::LeadingZero => {
                formatter.write_str("numeric version identifier has a leading zero")
            }
            Self::InvalidIdentifier => {
                formatter.write_str("prerelease or build identifier is invalid")
            }
            Self::TooManyPrereleaseIdentifiers { limit } => {
                write!(formatter, "prerelease has more than {limit} identifiers")
            }
        }
    }
}

impl Error for VersionError {}

fn split_once_unique(value: &str, separator: char) -> Result<(&str, Option<&str>), VersionError> {
    let Some((left, right)) = value.split_once(separator) else {
        return Ok((value, None));
    };
    if left.is_empty() || right.is_empty() || right.contains(separator) {
        return Err(VersionError::InvalidSyntax);
    }
    Ok((left, Some(right)))
}

fn parse_core_number(value: &str) -> Result<Box<str>, VersionError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(VersionError::InvalidCore);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(VersionError::LeadingZero);
    }
    Ok(value.into())
}

fn parse_prerelease(value: &str) -> Result<Box<[PrereleaseIdentifier]>, VersionError> {
    validate_dot_identifiers(value, true)?;
    let identifiers = value
        .split('.')
        .map(|identifier| {
            if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                PrereleaseIdentifier::Numeric(identifier.into())
            } else {
                PrereleaseIdentifier::Alphanumeric(identifier.into())
            }
        })
        .collect::<Vec<_>>();
    Ok(identifiers.into_boxed_slice())
}

fn validate_dot_identifiers(value: &str, prerelease: bool) -> Result<(), VersionError> {
    let mut count = 0_usize;
    for identifier in value.split('.') {
        count = count.saturating_add(1);
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(VersionError::InvalidIdentifier);
        }
        if prerelease
            && identifier.len() > 1
            && identifier.bytes().all(|byte| byte.is_ascii_digit())
            && identifier.starts_with('0')
        {
            return Err(VersionError::LeadingZero);
        }
    }
    if prerelease && count > MAX_PRERELEASE_IDENTIFIERS {
        return Err(VersionError::TooManyPrereleaseIdentifiers {
            limit: MAX_PRERELEASE_IDENTIFIERS,
        });
    }
    Ok(())
}

fn compare_prerelease(left: &[PrereleaseIdentifier], right: &[PrereleaseIdentifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    for (left, right) in left.iter().zip(right) {
        let ordering = compare_identifier(left, right);
        if !ordering.is_eq() {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn compare_identifier(left: &PrereleaseIdentifier, right: &PrereleaseIdentifier) -> Ordering {
    match (left, right) {
        (PrereleaseIdentifier::Numeric(left), PrereleaseIdentifier::Numeric(right)) => {
            compare_numeric_text(left, right)
        }
        (PrereleaseIdentifier::Numeric(_), PrereleaseIdentifier::Alphanumeric(_)) => Ordering::Less,
        (PrereleaseIdentifier::Alphanumeric(_), PrereleaseIdentifier::Numeric(_)) => {
            Ordering::Greater
        }
        (PrereleaseIdentifier::Alphanumeric(left), PrereleaseIdentifier::Alphanumeric(right)) => {
            left.cmp(right)
        }
    }
}

fn compare_numeric_text(left: &str, right: &str) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn release(tag: &str, kind: ReleaseKind) -> RemoteRelease<'_> {
        RemoteRelease::new(tag, kind)
    }

    #[test]
    fn embedded_running_version_is_valid_semantic_metadata() {
        let version = running_version().unwrap();
        assert_eq!(version.as_str(), RUNNING_VERSION);
    }

    #[test]
    fn parses_core_prerelease_and_build_metadata() {
        let version = SemanticVersion::parse("12.34.56-rc.2+macos.arm64").unwrap();
        assert_eq!(version.major(), "12");
        assert_eq!(version.minor(), "34");
        assert_eq!(version.patch(), "56");
        assert!(version.is_prerelease());
        assert_eq!(version.as_str(), "12.34.56-rc.2+macos.arm64");
    }

    #[test]
    fn remote_tags_accept_only_one_lowercase_v_prefix() {
        assert_eq!(
            SemanticVersion::parse_release_tag("v1.2.3")
                .unwrap()
                .as_str(),
            "1.2.3"
        );
        assert!(SemanticVersion::parse("v1.2.3").is_err());
        assert!(SemanticVersion::parse_release_tag("V1.2.3").is_err());
        assert!(SemanticVersion::parse_release_tag("vv1.2.3").is_err());
    }

    #[test]
    fn semantic_version_debug_redacts_validated_text() {
        let version = SemanticVersion::parse("1.2.3-secret-api-token+private-build").unwrap();
        let debug = format!("{version:?}");

        assert_eq!(
            debug,
            concat!(
                "SemanticVersion { bytes: 36, major_digits: 1, minor_digits: 1, ",
                "patch_digits: 1, prerelease_identifiers: 1 }"
            )
        );
        assert!(!debug.contains("secret-api-token"));
        assert!(!debug.contains("private-build"));
    }

    #[test]
    fn remote_release_debug_redacts_untrusted_tag_text() {
        let tag = "\u{1b}[31msecret-api-token";
        let release = release(tag, ReleaseKind::Nightly);
        let debug = format!("{release:?}");
        let tag_bytes = tag.len();

        assert_eq!(
            debug,
            format!("RemoteRelease {{ kind: Nightly, tag_bytes: {tag_bytes} }}")
        );
        assert!(!debug.contains("secret-api-token"));
    }

    #[test]
    fn invalid_remote_decision_debug_does_not_retain_tag_text() {
        let decision = decide_automatic_update(
            "1.0.0",
            release("secret-api-token", ReleaseKind::Stable),
            UpdateChannel::Stable,
        );
        let debug = format!("{decision:?}");

        assert!(matches!(
            decision,
            AutomaticUpdateDecision::InvalidRemoteVersion(_)
        ));
        assert!(!debug.contains("secret-api-token"));
    }

    #[test]
    fn semantic_prerelease_precedence_matches_the_standard_sequence() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ]
        .map(|value| SemanticVersion::parse(value).unwrap());

        for pair in ordered.windows(2) {
            assert!(pair[0].precedence_cmp(&pair[1]).is_lt());
        }
    }

    #[test]
    fn precedence_comparison_is_antisymmetric_and_transitive() {
        let versions = [
            "0.0.0",
            "1.0.0-alpha",
            "1.0.0-alpha.999999999999999999999999999999",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0",
            "999999999999999999999999999999.0.0",
        ]
        .map(|value| SemanticVersion::parse(value).unwrap());

        for (left_index, left) in versions.iter().enumerate() {
            for (right_index, right) in versions.iter().enumerate() {
                assert_eq!(
                    left.precedence_cmp(right),
                    right.precedence_cmp(left).reverse()
                );
                assert_eq!(left.precedence_cmp(right), left_index.cmp(&right_index));
            }
        }
        for first in 0..versions.len() {
            for second in first..versions.len() {
                for third in second..versions.len() {
                    assert!(!versions[first].precedence_cmp(&versions[second]).is_gt());
                    assert!(!versions[second].precedence_cmp(&versions[third]).is_gt());
                    assert!(!versions[first].precedence_cmp(&versions[third]).is_gt());
                }
            }
        }
    }

    #[test]
    fn prerelease_and_build_identifiers_may_contain_hyphens() {
        let version = SemanticVersion::parse("1.0.0-nightly-20260729+build-arm64").unwrap();
        assert!(version.is_prerelease());
        assert_eq!(version.as_str(), "1.0.0-nightly-20260729+build-arm64");
    }

    #[test]
    fn core_components_have_numeric_precedence() {
        let nine = SemanticVersion::parse("9.100.7").unwrap();
        let ten = SemanticVersion::parse("10.0.0").unwrap();
        assert!(nine.precedence_cmp(&ten).is_lt());
    }

    #[test]
    fn build_metadata_does_not_affect_precedence() {
        let first = SemanticVersion::parse("1.2.3+first").unwrap();
        let second = SemanticVersion::parse("1.2.3+second").unwrap();
        assert!(first.has_same_precedence(&second));
        assert!(!first.has_same_identity(&second));
    }

    #[test]
    fn rejects_malformed_versions() {
        for value in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.-3",
            "1.2.3-",
            "1.2.3-alpha..one",
            "1.2.3-alpha_1",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3+meta..one",
            "1.2.3+meta+again",
            " 1.2.3",
            "1.2.3 ",
            "1.2.3\n",
        ] {
            assert!(SemanticVersion::parse(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn prerelease_identifier_bound_and_build_numeric_zeroes_are_exact() {
        let maximum = format!(
            "1.0.0-{}+build.0001",
            std::iter::repeat_n("a", MAX_PRERELEASE_IDENTIFIERS)
                .collect::<Vec<_>>()
                .join(".")
        );
        assert!(SemanticVersion::parse(&maximum).is_ok());

        let excessive = format!(
            "1.0.0-{}",
            std::iter::repeat_n("a", MAX_PRERELEASE_IDENTIFIERS + 1)
                .collect::<Vec<_>>()
                .join(".")
        );
        assert!(matches!(
            SemanticVersion::parse(&excessive),
            Err(VersionError::TooManyPrereleaseIdentifiers {
                limit: MAX_PRERELEASE_IDENTIFIERS
            })
        ));
    }

    #[test]
    fn accepts_arbitrarily_large_core_numbers_within_the_input_bound() {
        let large = SemanticVersion::parse("18446744073709551616.0.0").unwrap();
        let smaller = SemanticVersion::parse("18446744073709551615.999.999").unwrap();
        assert_eq!(large.major(), "18446744073709551616");
        assert!(large.precedence_cmp(&smaller).is_gt());
    }

    #[test]
    fn rejects_only_input_beyond_the_explicit_total_bound() {
        let oversized = "1".repeat(MAX_VERSION_BYTES + 1);
        assert!(matches!(
            SemanticVersion::parse(&oversized),
            Err(VersionError::TooLong {
                bytes,
                limit: MAX_VERSION_BYTES,
            }) if bytes == MAX_VERSION_BYTES + 1
        ));
    }

    #[test]
    fn stable_channel_rejects_trusted_nightly_artifacts() {
        assert!(matches!(
            decide_automatic_update(
                "1.0.0",
                release("v1.1.0-nightly.1", ReleaseKind::Nightly),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::ChannelMismatch
        ));
    }

    #[test]
    fn nightly_channel_accepts_newer_prereleases_and_final_releases() {
        assert!(matches!(
            decide_automatic_update(
                "1.0.0",
                release("v1.1.0-nightly.1", ReleaseKind::Nightly),
                UpdateChannel::Nightly,
            ),
            AutomaticUpdateDecision::Available(_)
        ));
        assert!(matches!(
            decide_automatic_update(
                "1.1.0-nightly.1",
                release("v1.1.0-nightly.2", ReleaseKind::Nightly),
                UpdateChannel::Nightly,
            ),
            AutomaticUpdateDecision::Available(_)
        ));
        assert!(matches!(
            decide_automatic_update(
                "1.1.0-nightly.1",
                release("v1.1.0", ReleaseKind::Stable),
                UpdateChannel::Nightly,
            ),
            AutomaticUpdateDecision::Available(_)
        ));
    }

    #[test]
    fn release_metadata_and_version_form_must_agree() {
        assert!(matches!(
            decide_automatic_update(
                "1.0.0",
                release("v1.1.0+nightly.20260729", ReleaseKind::Nightly),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::InvalidRemoteMetadata
        ));
        assert!(matches!(
            decide_automatic_update(
                "1.0.0",
                release("v1.1.0-rc.1", ReleaseKind::Stable),
                UpdateChannel::Nightly,
            ),
            AutomaticUpdateDecision::InvalidRemoteMetadata
        ));
    }

    #[test]
    fn equal_build_metadata_and_older_versions_do_not_update() {
        assert!(matches!(
            decide_automatic_update(
                "1.2.3+local",
                release("v1.2.3+remote", ReleaseKind::Stable),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::Current
        ));
        assert!(matches!(
            decide_automatic_update(
                "2.0.0",
                release("v1.99.99", ReleaseKind::Stable),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::Current
        ));
    }

    #[test]
    fn development_and_empty_builds_never_notify_automatically() {
        for current in ["", "dev", "DEV"] {
            assert!(matches!(
                decide_automatic_update(
                    current,
                    release("v999.0.0", ReleaseKind::Stable),
                    UpdateChannel::Stable,
                ),
                AutomaticUpdateDecision::DevelopmentBuild
            ));
        }
    }

    #[test]
    fn malformed_values_fail_closed() {
        assert!(matches!(
            decide_automatic_update(
                "not-a-version",
                release("v2.0.0", ReleaseKind::Stable),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::InvalidCurrentVersion(_)
        ));
        assert!(matches!(
            decide_automatic_update(
                "1.0.0",
                release("latest", ReleaseKind::Stable),
                UpdateChannel::Stable,
            ),
            AutomaticUpdateDecision::InvalidRemoteVersion(_)
        ));
    }
}
