//! Bounded GitHub release discovery and verified manual-update transport.
//!
//! Release metadata is treated as untrusted until its channel, semantic
//! version, asset set, and checksum record are validated. Downloaded executable
//! bytes stream directly into the descriptor-anchored update boundary; they are
//! never executed or published before checksum verification.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::UpdateChannel;
use crate::update_apply::{
    TrustedReleaseArtifact, UpdateApplyError, UpdateApplyOutcome, apply_update_from_reader,
};
use crate::updater::UPDATE_REQUEST_TIMEOUT;
use crate::version::{ReleaseKind, RemoteRelease, SemanticVersion};

/// Repository used by official installers and update checks.
pub const DEFAULT_RELEASE_REPOSITORY: &str = "rselbach/argmax";
/// Maximum retained GitHub release response.
pub const MAX_RELEASE_METADATA_BYTES: usize = 256 * 1024;
/// Maximum retained per-asset checksum response.
pub const MAX_CHECKSUM_BYTES: usize = 4 * 1024;
/// Manual update deadline, including metadata, checksum, and artifact transfer.
pub const MANUAL_UPDATE_TIMEOUT: Duration = Duration::from_secs(120);

const MAX_RELEASE_ASSETS: usize = 64;
const MAX_RELEASE_NAME_BYTES: usize = 512;
const MAX_ASSET_NAME_BYTES: usize = 256;
const USER_AGENT: &str = "argmax-updater";
const NIGHTLY_TAG: &str = "nightly";
const NIGHTLY_TITLE_PREFIX: &str = "argmax nightly ";

/// Validated GitHub repository used to construct release endpoints.
#[derive(Clone, Eq, PartialEq)]
pub struct ReleaseSource {
    repository: Box<str>,
}

impl ReleaseSource {
    /// Creates a source from one ASCII `owner/repository` pair.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseError::InvalidSource`] for an empty, oversized, or
    /// structurally unsafe repository value.
    pub fn new(repository: &str) -> Result<Self, ReleaseError> {
        if repository.is_empty()
            || repository.len() > MAX_ASSET_NAME_BYTES
            || repository.starts_with('/')
            || repository.ends_with('/')
            || repository.matches('/').count() != 1
            || repository.contains("//")
            || !repository.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
            })
            // A relative component would be resolved away by the request URL,
            // making the value name an endpoint other than the repository it
            // appears to name.
            || repository
                .split('/')
                .any(|component| matches!(component, "." | ".."))
        {
            return Err(ReleaseError::InvalidSource);
        }
        Ok(Self {
            repository: repository.into(),
        })
    }

    /// Official argmax release source.
    #[must_use]
    pub fn official() -> Self {
        Self {
            repository: DEFAULT_RELEASE_REPOSITORY.into(),
        }
    }

    fn api_url(&self, endpoint: ReleaseEndpoint) -> String {
        let endpoint = match endpoint {
            ReleaseEndpoint::Stable => "latest",
            ReleaseEndpoint::Nightly => "tags/nightly",
        };
        format!(
            "https://api.github.com/repos/{}/releases/{endpoint}",
            self.repository
        )
    }

    fn download_url(&self, tag: &str, asset: &str) -> String {
        format!(
            "https://github.com/{}/releases/download/{tag}/{asset}",
            self.repository
        )
    }
}

impl fmt::Debug for ReleaseSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseSource")
            .field("repository_bytes", &self.repository.len())
            .finish()
    }
}

/// One fully validated release and host asset selection.
pub struct ReleaseDescriptor {
    version: SemanticVersion,
    kind: ReleaseKind,
    tag: Box<str>,
    asset: &'static str,
}

impl ReleaseDescriptor {
    /// Semantic release version without a tag prefix.
    #[must_use]
    pub fn version(&self) -> &str {
        self.version.as_str()
    }

    /// Trusted stable/nightly classification.
    #[must_use]
    pub const fn kind(&self) -> ReleaseKind {
        self.kind
    }

    /// Borrowed input for the automatic-update policy state machine.
    #[must_use]
    pub fn remote_release(&self) -> RemoteRelease<'_> {
        RemoteRelease::new(self.version.as_str(), self.kind)
    }

    fn checksum_asset(&self) -> String {
        format!("{}.sha256", self.asset)
    }

    fn checksum_url(&self, source: &ReleaseSource) -> String {
        source.download_url(&self.tag, &self.checksum_asset())
    }

    fn artifact_url(&self, source: &ReleaseSource) -> String {
        source.download_url(&self.tag, self.asset)
    }
}

impl fmt::Debug for ReleaseDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseDescriptor")
            .field("version_bytes", &self.version.as_str().len())
            .field("kind", &self.kind)
            .field("tag_bytes", &self.tag.len())
            .field("asset", &self.asset)
            .finish()
    }
}

/// Sanitized release transport or metadata failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseError {
    /// Repository selection was malformed.
    InvalidSource,
    /// Host operating system or architecture has no release artifact.
    UnsupportedHost,
    /// The complete request deadline elapsed.
    Timeout,
    /// DNS, TLS, connection, or response streaming failed.
    Network,
    /// GitHub rejected the request because its unauthenticated limit was reached.
    RateLimited,
    /// Release service returned another unsuccessful status.
    HttpStatus(u16),
    /// A bounded response exceeded its accepted size.
    ResponseTooLarge,
    /// Release JSON, channel metadata, version, or asset names were invalid.
    InvalidMetadata,
    /// The required host executable or checksum asset was absent or duplicated.
    MissingAsset,
    /// A checksum response did not bind one lowercase SHA-256 value to the asset.
    InvalidChecksum,
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSource => formatter.write_str("release repository is invalid"),
            Self::UnsupportedHost => {
                formatter.write_str("no release artifact supports this operating system and CPU")
            }
            Self::Timeout => formatter.write_str("release request timed out"),
            Self::Network => formatter.write_str("release service could not be reached"),
            Self::RateLimited => formatter.write_str("release service rate limit was reached"),
            Self::HttpStatus(status) => {
                write!(formatter, "release service returned HTTP status {status}")
            }
            Self::ResponseTooLarge => formatter.write_str("release response exceeded its limit"),
            Self::InvalidMetadata => formatter.write_str("release metadata was invalid"),
            Self::MissingAsset => formatter.write_str("release is missing the required host asset"),
            Self::InvalidChecksum => formatter.write_str("release checksum record was invalid"),
        }
    }
}

impl Error for ReleaseError {}

/// Result of an explicit update command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManualUpdateOutcome {
    /// Running executable has equal or newer semantic precedence.
    AlreadyCurrent {
        /// Validated candidate version.
        version: Box<str>,
    },
    /// A verified executable was atomically published.
    Updated {
        /// Installed version.
        version: Box<str>,
        /// Atomic updater's cleanup disposition.
        apply: UpdateApplyOutcome,
    },
}

/// Result of release discovery before any artifact bytes are downloaded.
pub enum ManualUpdateCheck {
    /// Running executable has equal or newer semantic precedence.
    AlreadyCurrent {
        /// Validated candidate version.
        version: Box<str>,
    },
    /// A newer release is ready for explicit verified installation.
    Available(ManualUpdatePlan),
}

impl fmt::Debug for ManualUpdateCheck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyCurrent { version } => formatter
                .debug_struct("AlreadyCurrent")
                .field("version", version)
                .finish(),
            Self::Available(plan) => formatter.debug_tuple("Available").field(plan).finish(),
        }
    }
}

/// Validated manual-update release retained across the user-visible
/// availability boundary.
pub struct ManualUpdatePlan {
    source: ReleaseSource,
    release: ReleaseDescriptor,
    deadline: Instant,
}

impl ManualUpdatePlan {
    /// Available semantic version without a tag prefix.
    #[must_use]
    pub fn version(&self) -> &str {
        self.release.version()
    }

    /// Downloads, verifies, and atomically installs this exact checked release.
    ///
    /// # Errors
    ///
    /// Returns a sanitized transport, verification, or atomic-apply failure.
    pub fn apply(
        self,
        current_executable: &Path,
    ) -> Result<ManualUpdateOutcome, ManualUpdateError> {
        let checksum_response = get(
            &self.release.checksum_url(&self.source),
            self.deadline,
            RedirectPolicy::Downloads,
        )?;
        let checksum_bytes = read_bounded(checksum_response, MAX_CHECKSUM_BYTES, self.deadline)?;
        let checksum = parse_checksum(&checksum_bytes, self.release.asset)?;
        let (operating_system, architecture) = host_identity()?;
        let trusted = TrustedReleaseArtifact::new(
            self.release.version(),
            operating_system,
            architecture,
            checksum,
        )
        .map_err(|_| ReleaseError::InvalidMetadata)?;

        let response = get(
            &self.release.artifact_url(&self.source),
            self.deadline,
            RedirectPolicy::Downloads,
        )?;
        let mut reader = DeadlineReader::new(response.into_body().into_reader(), self.deadline);
        let apply = apply_update_from_reader(&trusted, &mut reader, current_executable)?;
        Ok(ManualUpdateOutcome::Updated {
            version: self.release.version().into(),
            apply,
        })
    }
}

impl fmt::Debug for ManualUpdatePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManualUpdatePlan")
            .field("source", &self.source)
            .field("release", &self.release)
            .finish_non_exhaustive()
    }
}

/// Explicit update failure with no retained remote content.
#[derive(Debug)]
pub enum ManualUpdateError {
    /// Release selection, transport, or checksum metadata failed.
    Release(ReleaseError),
    /// Current executable could not be replaced safely.
    Apply(UpdateApplyError),
}

impl fmt::Display for ManualUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release(error) => error.fmt(formatter),
            Self::Apply(error) => error.fmt(formatter),
        }
    }
}

impl Error for ManualUpdateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Release(error) => Some(error),
            Self::Apply(error) => Some(error),
        }
    }
}

impl From<ReleaseError> for ManualUpdateError {
    fn from(error: ReleaseError) -> Self {
        Self::Release(error)
    }
}

impl From<UpdateApplyError> for ManualUpdateError {
    fn from(error: UpdateApplyError) -> Self {
        Self::Apply(error)
    }
}

/// Fetches the newest release eligible for one configured channel.
///
/// A stable check reads only GitHub's latest stable release. A nightly check
/// compares that release with the rolling `nightly` prerelease and uses the
/// greater semantic precedence. The timeout covers all requests together.
///
/// # Errors
///
/// Returns a sanitized transport, metadata, or host-selection failure.
pub fn fetch_release(
    source: &ReleaseSource,
    channel: UpdateChannel,
    timeout: Duration,
) -> Result<ReleaseDescriptor, ReleaseError> {
    if timeout.is_zero() {
        return Err(ReleaseError::Timeout);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ReleaseError::Timeout)?;
    let asset = host_asset_name()?;
    let stable = fetch_endpoint(source, ReleaseEndpoint::Stable, asset, deadline);
    if channel == UpdateChannel::Stable {
        return stable;
    }
    let nightly = fetch_endpoint(source, ReleaseEndpoint::Nightly, asset, deadline);
    choose_nightly_candidate(stable, nightly)
}

/// Performs the five-second transport portion of an automatic update check.
///
/// This function is blocking and must run away from terminal input forwarding.
///
/// # Errors
///
/// Returns a sanitized release failure. Interactive callers normally record
/// and suppress this result through the automatic-updater state machine.
pub fn fetch_automatic_release(
    source: &ReleaseSource,
    channel: UpdateChannel,
) -> Result<ReleaseDescriptor, ReleaseError> {
    fetch_release(source, channel, UPDATE_REQUEST_TIMEOUT)
}

/// Checks, downloads, verifies, and atomically installs an explicit update.
///
/// Development or malformed local versions may be explicitly replaced; only
/// automatic notices suppress them. A valid running version that is equal to
/// or newer than the selected release produces [`ManualUpdateOutcome::AlreadyCurrent`].
///
/// # Errors
///
/// Returns a sanitized release-transport error or the descriptor-anchored
/// atomic updater's failure. No unverified bytes replace the executable.
pub fn apply_manual_update(
    source: &ReleaseSource,
    channel: UpdateChannel,
    running_version: &str,
    current_executable: &Path,
) -> Result<ManualUpdateOutcome, ManualUpdateError> {
    match check_manual_update(source, channel, running_version)? {
        ManualUpdateCheck::AlreadyCurrent { version } => {
            Ok(ManualUpdateOutcome::AlreadyCurrent { version })
        }
        ManualUpdateCheck::Available(plan) => plan.apply(current_executable),
    }
}

/// Checks for an explicit update without downloading or modifying an artifact.
///
/// The returned plan is bound to the exact validated release and the original
/// two-minute deadline, so callers can report availability before installation
/// without repeating discovery or racing to a different release.
///
/// # Errors
///
/// Returns a sanitized release-discovery or host-selection failure.
pub fn check_manual_update(
    source: &ReleaseSource,
    channel: UpdateChannel,
    running_version: &str,
) -> Result<ManualUpdateCheck, ReleaseError> {
    let deadline = Instant::now()
        .checked_add(MANUAL_UPDATE_TIMEOUT)
        .ok_or(ReleaseError::Timeout)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let release = fetch_release(source, channel, remaining)?;
    if SemanticVersion::parse(running_version)
        .is_ok_and(|current| !release.version.precedence_cmp(&current).is_gt())
    {
        return Ok(ManualUpdateCheck::AlreadyCurrent {
            version: release.version().into(),
        });
    }
    Ok(ManualUpdateCheck::Available(ManualUpdatePlan {
        source: source.clone(),
        release,
        deadline,
    }))
}

#[derive(Clone, Copy)]
enum ReleaseEndpoint {
    Stable,
    Nightly,
}

fn fetch_endpoint(
    source: &ReleaseSource,
    endpoint: ReleaseEndpoint,
    asset: &'static str,
    deadline: Instant,
) -> Result<ReleaseDescriptor, ReleaseError> {
    let response = get(
        &source.api_url(endpoint),
        deadline,
        RedirectPolicy::Metadata,
    )?;
    let bytes = read_bounded(response, MAX_RELEASE_METADATA_BYTES, deadline)?;
    parse_release(&bytes, endpoint, asset)
}

fn choose_nightly_candidate(
    stable: Result<ReleaseDescriptor, ReleaseError>,
    nightly: Result<ReleaseDescriptor, ReleaseError>,
) -> Result<ReleaseDescriptor, ReleaseError> {
    match (stable, nightly) {
        (Ok(stable), Ok(nightly)) => {
            if stable.version.precedence_cmp(&nightly.version) == Ordering::Greater {
                Ok(stable)
            } else {
                Ok(nightly)
            }
        }
        (Ok(release), Err(_)) | (Err(_), Ok(release)) => Ok(release),
        (Err(ReleaseError::RateLimited), Err(_)) | (Err(_), Err(ReleaseError::RateLimited)) => {
            Err(ReleaseError::RateLimited)
        }
        (Err(ReleaseError::Timeout), Err(_)) | (Err(_), Err(ReleaseError::Timeout)) => {
            Err(ReleaseError::Timeout)
        }
        (Err(error), Err(_)) => Err(error),
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
}

fn parse_release(
    bytes: &[u8],
    endpoint: ReleaseEndpoint,
    asset: &'static str,
) -> Result<ReleaseDescriptor, ReleaseError> {
    let release: GithubRelease =
        serde_json::from_slice(bytes).map_err(|_| ReleaseError::InvalidMetadata)?;
    if release.draft
        || release.assets.len() > MAX_RELEASE_ASSETS
        || release.tag_name.len() > MAX_RELEASE_NAME_BYTES
        || release
            .name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_RELEASE_NAME_BYTES)
        || release
            .assets
            .iter()
            .any(|candidate| candidate.name.len() > MAX_ASSET_NAME_BYTES)
    {
        return Err(ReleaseError::InvalidMetadata);
    }

    let (version, kind, tag) = match endpoint {
        ReleaseEndpoint::Stable => {
            if release.prerelease {
                return Err(ReleaseError::InvalidMetadata);
            }
            let version = SemanticVersion::parse_release_tag(&release.tag_name)
                .map_err(|_| ReleaseError::InvalidMetadata)?;
            if version.is_prerelease() {
                return Err(ReleaseError::InvalidMetadata);
            }
            (version, ReleaseKind::Stable, release.tag_name)
        }
        ReleaseEndpoint::Nightly => {
            if !release.prerelease || release.tag_name != NIGHTLY_TAG {
                return Err(ReleaseError::InvalidMetadata);
            }
            let name = release.name.ok_or(ReleaseError::InvalidMetadata)?;
            let version_text = name
                .strip_prefix(NIGHTLY_TITLE_PREFIX)
                .ok_or(ReleaseError::InvalidMetadata)?;
            let version =
                SemanticVersion::parse(version_text).map_err(|_| ReleaseError::InvalidMetadata)?;
            if !version.is_prerelease() {
                return Err(ReleaseError::InvalidMetadata);
            }
            (version, ReleaseKind::Nightly, release.tag_name)
        }
    };

    let checksum = format!("{asset}.sha256");
    let asset_count = release
        .assets
        .iter()
        .filter(|candidate| candidate.name == asset)
        .count();
    let checksum_count = release
        .assets
        .iter()
        .filter(|candidate| candidate.name == checksum)
        .count();
    if asset_count != 1 || checksum_count != 1 {
        return Err(ReleaseError::MissingAsset);
    }

    Ok(ReleaseDescriptor {
        version,
        kind,
        tag: tag.into(),
        asset,
    })
}

fn host_asset_name() -> Result<&'static str, ReleaseError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("argmax-linux-amd64"),
        ("linux", "aarch64") => Ok("argmax-linux-arm64"),
        ("macos", "x86_64") => Ok("argmax-macos-amd64"),
        ("macos", "aarch64") => Ok("argmax-macos-arm64"),
        _ => Err(ReleaseError::UnsupportedHost),
    }
}

fn host_identity() -> Result<(&'static str, &'static str), ReleaseError> {
    let operating_system = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        _ => return Err(ReleaseError::UnsupportedHost),
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err(ReleaseError::UnsupportedHost),
    };
    Ok((operating_system, architecture))
}

#[derive(Clone, Copy)]
enum RedirectPolicy {
    Metadata,
    Downloads,
}

fn get(
    url: &str,
    deadline: Instant,
    redirect_policy: RedirectPolicy,
) -> Result<ureq::http::Response<ureq::Body>, ReleaseError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ReleaseError::Timeout);
    }
    let redirects = match redirect_policy {
        RedirectPolicy::Metadata => 0,
        RedirectPolicy::Downloads => 5,
    };
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .https_only(true)
        .max_redirects(redirects)
        .http_status_as_error(false)
        .timeout_global(Some(remaining))
        .build();
    let agent: ureq::Agent = config.into();
    let response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(classify_transport_error)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(response),
        403 | 429 => Err(ReleaseError::RateLimited),
        _ => Err(ReleaseError::HttpStatus(status)),
    }
}

fn classify_transport_error(error: ureq::Error) -> ReleaseError {
    match error {
        ureq::Error::Timeout(_) => ReleaseError::Timeout,
        ureq::Error::Io(error) if error.kind() == io::ErrorKind::TimedOut => ReleaseError::Timeout,
        ureq::Error::StatusCode(403 | 429) => ReleaseError::RateLimited,
        ureq::Error::StatusCode(status) => ReleaseError::HttpStatus(status),
        ureq::Error::BodyExceedsLimit(_) => ReleaseError::ResponseTooLarge,
        _ => ReleaseError::Network,
    }
}

fn read_bounded(
    response: ureq::http::Response<ureq::Body>,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>, ReleaseError> {
    let read_limit = limit.checked_add(1).ok_or(ReleaseError::ResponseTooLarge)?;
    let mut reader = DeadlineReader::new(response.into_body().into_reader(), deadline);
    let mut bytes = Vec::with_capacity(read_limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let remaining = read_limit.saturating_sub(bytes.len());
        if remaining == 0 {
            return Err(ReleaseError::ResponseTooLarge);
        }
        let chunk_limit = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..chunk_limit]).map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut {
                ReleaseError::Timeout
            } else {
                ReleaseError::Network
            }
        })?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > limit {
            return Err(ReleaseError::ResponseTooLarge);
        }
    }
}

struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}

impl<R> DeadlineReader<R> {
    const fn new(inner: R, deadline: Instant) -> Self {
        Self { inner, deadline }
    }
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "release request deadline elapsed",
            ));
        }
        self.inner.read(buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut
                || matches!(
                    error
                        .get_ref()
                        .and_then(|source| source.downcast_ref::<ureq::Error>()),
                    Some(ureq::Error::Timeout(_))
                )
            {
                io::Error::new(io::ErrorKind::TimedOut, "release request deadline elapsed")
            } else {
                io::Error::new(io::ErrorKind::ConnectionAborted, "release response failed")
            }
        })
    }
}

fn parse_checksum<'a>(bytes: &'a [u8], asset: &str) -> Result<&'a str, ReleaseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ReleaseError::InvalidChecksum)?;
    let mut matching = None;
    for line in text.lines() {
        let Some((digest, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name
            .trim_start()
            .strip_prefix('*')
            .unwrap_or(name.trim_start());
        if name != asset {
            continue;
        }
        if matching.is_some()
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ReleaseError::InvalidChecksum);
        }
        matching = Some(digest);
    }
    matching.ok_or(ReleaseError::InvalidChecksum)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSET: &str = "argmax-linux-amd64";

    fn release_json(tag: &str, name: &str, draft: bool, prerelease: bool) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "tag_name": tag,
            "name": name,
            "draft": draft,
            "prerelease": prerelease,
            "assets": [
                { "name": ASSET },
                { "name": format!("{ASSET}.sha256") }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn repository_and_urls_accept_only_one_safe_pair() {
        let source = ReleaseSource::new("greendale/argmax").unwrap();
        assert_eq!(
            source.api_url(ReleaseEndpoint::Stable),
            "https://api.github.com/repos/greendale/argmax/releases/latest"
        );
        assert_eq!(
            source.download_url("v1.2.3", ASSET),
            "https://github.com/greendale/argmax/releases/download/v1.2.3/argmax-linux-amd64"
        );
        for invalid in [
            "", "argmax", "/argmax", "a/b/c", "a//b", "a/b?x", "a/..", "../b", "./b", "a/.",
        ] {
            assert_eq!(
                ReleaseSource::new(invalid).unwrap_err(),
                ReleaseError::InvalidSource
            );
        }
    }

    #[test]
    fn stable_release_requires_final_version_and_exact_assets() {
        let descriptor = parse_release(
            &release_json("v1.2.3", "argmax 1.2.3", false, false),
            ReleaseEndpoint::Stable,
            ASSET,
        )
        .unwrap();
        assert_eq!(descriptor.version(), "1.2.3");
        assert_eq!(descriptor.kind(), ReleaseKind::Stable);

        for bytes in [
            release_json("v1.2.3-rc.1", "candidate", false, false),
            release_json("v1.2.3", "draft", true, false),
            release_json("v1.2.3", "prerelease", false, true),
        ] {
            assert_eq!(
                parse_release(&bytes, ReleaseEndpoint::Stable, ASSET).unwrap_err(),
                ReleaseError::InvalidMetadata
            );
        }
    }

    #[test]
    fn rolling_nightly_version_comes_from_strict_release_title() {
        let descriptor = parse_release(
            &release_json(
                "nightly",
                "argmax nightly 1.2.3-nightly.20260729",
                false,
                true,
            ),
            ReleaseEndpoint::Nightly,
            ASSET,
        )
        .unwrap();
        assert_eq!(descriptor.version(), "1.2.3-nightly.20260729");
        assert_eq!(descriptor.kind(), ReleaseKind::Nightly);

        for name in [
            "1.2.3-nightly.1",
            "argmax nightly 1.2.3",
            "argmax nightly bad",
        ] {
            assert_eq!(
                parse_release(
                    &release_json("nightly", name, false, true),
                    ReleaseEndpoint::Nightly,
                    ASSET,
                )
                .unwrap_err(),
                ReleaseError::InvalidMetadata
            );
        }
    }

    #[test]
    fn missing_and_duplicate_assets_fail_closed() {
        let missing = serde_json::to_vec(&serde_json::json!({
            "tag_name": "v1.2.3",
            "name": "argmax 1.2.3",
            "draft": false,
            "prerelease": false,
            "assets": [{ "name": ASSET }]
        }))
        .unwrap();
        assert_eq!(
            parse_release(&missing, ReleaseEndpoint::Stable, ASSET).unwrap_err(),
            ReleaseError::MissingAsset
        );

        let duplicate = serde_json::to_vec(&serde_json::json!({
            "tag_name": "v1.2.3",
            "name": "argmax 1.2.3",
            "draft": false,
            "prerelease": false,
            "assets": [
                { "name": ASSET },
                { "name": ASSET },
                { "name": format!("{ASSET}.sha256") }
            ]
        }))
        .unwrap();
        assert_eq!(
            parse_release(&duplicate, ReleaseEndpoint::Stable, ASSET).unwrap_err(),
            ReleaseError::MissingAsset
        );
    }

    #[test]
    fn checksum_must_bind_one_exact_asset() {
        let hash = "a".repeat(64);
        let line = format!("{hash}  {ASSET}\n");
        assert_eq!(parse_checksum(line.as_bytes(), ASSET).unwrap(), hash);
        let executable = format!("{hash} *{ASSET}\n");
        assert_eq!(parse_checksum(executable.as_bytes(), ASSET).unwrap(), hash);
        for invalid in [
            format!("{hash}  argmax-linux-arm64\n"),
            format!("{}  {ASSET}\n", "A".repeat(64)),
            format!("{hash}  {ASSET}\n{hash}  {ASSET}\n"),
        ] {
            assert_eq!(
                parse_checksum(invalid.as_bytes(), ASSET).unwrap_err(),
                ReleaseError::InvalidChecksum
            );
        }
    }

    #[test]
    fn nightly_channel_uses_newest_successful_candidate() {
        let stable = parse_release(
            &release_json("v1.2.3", "stable", false, false),
            ReleaseEndpoint::Stable,
            ASSET,
        );
        let nightly = parse_release(
            &release_json("nightly", "argmax nightly 1.3.0-nightly.1", false, true),
            ReleaseEndpoint::Nightly,
            ASSET,
        );
        assert_eq!(
            choose_nightly_candidate(stable, nightly).unwrap().version(),
            "1.3.0-nightly.1"
        );
        let fallback = choose_nightly_candidate(
            parse_release(
                &release_json("v2.0.0", "stable", false, false),
                ReleaseEndpoint::Stable,
                ASSET,
            ),
            Err(ReleaseError::HttpStatus(404)),
        )
        .unwrap();
        assert_eq!(fallback.version(), "2.0.0");
    }

    #[test]
    fn debug_output_never_exposes_repository_or_remote_version() {
        let source = ReleaseSource::new("secret-owner/secret-repository").unwrap();
        let descriptor = parse_release(
            &release_json("v9.8.7", "secret title", false, false),
            ReleaseEndpoint::Stable,
            ASSET,
        )
        .unwrap();
        let debug = format!("{source:?} {descriptor:?}");
        assert!(!debug.contains("secret-owner"));
        assert!(!debug.contains("9.8.7"));
    }
}
