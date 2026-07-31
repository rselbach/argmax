//! Verified, descriptor-anchored executable replacement for manual updates.
//!
//! Network access and release selection deliberately remain outside this
//! module. Callers provide trusted, validated release metadata and downloaded
//! bytes; this module never executes those bytes.
//!
//! Transaction pathnames are intentionally retained. POSIX does not provide an
//! inode-conditional unlink, so deleting a checked pathname could remove an
//! unrelated file substituted between the check and `unlinkat`. Retention is
//! bounded to one random entry per attempt and is surfaced for successful
//! publication; a failed attempt after transaction creation may likewise leave
//! its private entry for explicit, separately authorized cleanup.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::version::{SemanticVersion, VersionError};

/// Maximum accepted executable artifact size (128 MiB).
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const RANDOM_NAME_BYTES: usize = 16;
const MAX_TEMP_NAME_ATTEMPTS: usize = 64;
const PRIVATE_FILE_MODE: u32 = 0o700;
const GROUP_OR_OTHER_WRITE_BITS: u32 = 0o022;
const SET_USER_OR_GROUP_ID_BITS: u32 = 0o6000;
const FILE_TYPE_MASK: u32 = 0o170_000;
const REGULAR_FILE_TYPE: u32 = 0o100_000;

/// Operating-system identity encoded by trusted release metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactOperatingSystem {
    /// Linux release artifact.
    Linux,
    /// macOS release artifact.
    Macos,
}

impl ArtifactOperatingSystem {
    fn parse(value: &str) -> Result<Self, ReleaseMetadataError> {
        match value {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            _ => Err(ReleaseMetadataError::InvalidOperatingSystem),
        }
    }
}

/// CPU-architecture identity encoded by trusted release metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactArchitecture {
    /// 64-bit x86 artifact, named `amd64` in release metadata.
    Amd64,
    /// 64-bit Arm artifact, named `arm64` in release metadata.
    Arm64,
}

impl ArtifactArchitecture {
    fn parse(value: &str) -> Result<Self, ReleaseMetadataError> {
        match value {
            "amd64" => Ok(Self::Amd64),
            "arm64" => Ok(Self::Arm64),
            _ => Err(ReleaseMetadataError::InvalidArchitecture),
        }
    }
}

/// Validated release identity and checksum supplied by a trusted release feed.
pub struct TrustedReleaseArtifact {
    version: SemanticVersion,
    operating_system: ArtifactOperatingSystem,
    architecture: ArtifactArchitecture,
    sha256: [u8; 32],
}

impl TrustedReleaseArtifact {
    /// Validates strict semantic-version, platform, and lowercase SHA-256 text.
    ///
    /// Operating-system and architecture names are deliberately canonical:
    /// `linux` or `macos`, and `amd64` or `arm64`. The version must not have a
    /// release-tag prefix. The checksum must be exactly 64 lowercase hexadecimal
    /// characters.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`ReleaseMetadataError`] for malformed metadata.
    pub fn new(
        version: &str,
        operating_system: &str,
        architecture: &str,
        sha256: &str,
    ) -> Result<Self, ReleaseMetadataError> {
        Ok(Self {
            version: SemanticVersion::parse(version)
                .map_err(ReleaseMetadataError::InvalidVersion)?,
            operating_system: ArtifactOperatingSystem::parse(operating_system)?,
            architecture: ArtifactArchitecture::parse(architecture)?,
            sha256: parse_sha256(sha256)?,
        })
    }

    /// Validated semantic-version text.
    #[must_use]
    pub fn version(&self) -> &str {
        self.version.as_str()
    }

    /// Trusted artifact operating system.
    #[must_use]
    pub const fn operating_system(&self) -> ArtifactOperatingSystem {
        self.operating_system
    }

    /// Trusted artifact architecture.
    #[must_use]
    pub const fn architecture(&self) -> ArtifactArchitecture {
        self.architecture
    }

    fn validate_host_identity(&self) -> Result<(), UpdateApplyError> {
        let operating_system = current_operating_system()?;
        let architecture = current_architecture()?;
        if self.operating_system != operating_system || self.architecture != architecture {
            return Err(UpdateApplyError::ArtifactTargetMismatch);
        }
        Ok(())
    }
}

impl fmt::Debug for TrustedReleaseArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedReleaseArtifact")
            .field("version_bytes", &self.version.as_str().len())
            .field("operating_system", &self.operating_system)
            .field("architecture", &self.architecture)
            .field("has_sha256", &true)
            .finish_non_exhaustive()
    }
}

/// Why trusted release metadata was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseMetadataError {
    /// The version was not strict semantic-version text.
    InvalidVersion(VersionError),
    /// The operating-system identity was not canonical and supported.
    InvalidOperatingSystem,
    /// The architecture identity was not canonical and supported.
    InvalidArchitecture,
    /// The checksum was not exactly 64 lowercase hexadecimal characters.
    InvalidSha256,
}

impl fmt::Display for ReleaseMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion(error) => write!(formatter, "invalid release version: {error}"),
            Self::InvalidOperatingSystem => {
                formatter.write_str("invalid release operating-system identity")
            }
            Self::InvalidArchitecture => {
                formatter.write_str("invalid release architecture identity")
            }
            Self::InvalidSha256 => formatter.write_str("invalid release SHA-256 checksum"),
        }
    }
}

impl Error for ReleaseMetadataError {}

/// Cleanup state after a successfully published update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviousExecutableCleanup {
    /// The verified replacement is durable, but the displaced executable was
    /// retained under a random transaction name.
    Retained,
}

/// Result of applying one verified artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateApplyOutcome {
    /// The current executable already had the trusted artifact checksum. The
    /// verified downloaded transaction is retained at its random private name;
    /// POSIX has no inode-conditional unlink primitive that could remove it
    /// without risking an unrelated concurrent replacement.
    AlreadyCurrent,
    /// The trusted artifact replaced the current executable atomically.
    Updated {
        /// Disposition of the displaced executable after durable publication.
        cleanup: PreviousExecutableCleanup,
    },
}

/// Closed, content-free manual-update failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateApplyError {
    /// This build does not target a supported release platform.
    UnsupportedHost,
    /// Trusted metadata selected an artifact for another supported platform.
    ArtifactTargetMismatch,
    /// A supplied filesystem path was not absolute.
    RelativePath,
    /// A supplied path lacked a usable parent or filename.
    MissingPathComponent,
    /// A path component was a symlink or was not a directory.
    UnsafeParentPath,
    /// The final installation directory was writable by its group or by other
    /// users, allowing another principal to replace update pathnames.
    UnsafeParentPermissions,
    /// An update directory or file carried a macOS extended access-control
    /// list whose mutation or inheritance semantics cannot be preserved safely.
    UnsafeAccessControl,
    /// The current executable was a symlink, non-regular file, hard link, or
    /// carried writable or privileged executable permissions.
    UnsafeCurrentExecutable,
    /// A downloaded artifact path or descriptor was not a safely permissioned,
    /// exclusive regular file.
    UnsafeDownloadedArtifact,
    /// The downloaded artifact was empty.
    EmptyArtifact,
    /// The downloaded artifact exceeded [`MAX_UPDATE_ARTIFACT_BYTES`].
    ArtifactTooLarge,
    /// The current executable exceeded [`MAX_UPDATE_ARTIFACT_BYTES`].
    CurrentExecutableTooLarge,
    /// Downloaded bytes did not match trusted release metadata.
    ChecksumMismatch,
    /// The current executable or its parent changed before publication.
    SourceChanged,
    /// The filesystem cannot perform the required atomic exchange.
    AtomicReplacementUnavailable,
    /// Publication occurred, but a concurrent mutation made the final pathname
    /// state ambiguous. No ambiguous pathname is removed.
    PublicationUncertain,
    /// The replacement was atomically published, but syncing its parent failed.
    ///
    /// The new executable may already be visible and the verified prior
    /// executable is retained under the random transaction name. Retrying is
    /// safe, but callers must report this as an ambiguous durability result,
    /// not as an ordinary pre-publication failure.
    PostPublicationSync {
        /// Content-free operating-system error category.
        kind: io::ErrorKind,
    },
    /// A filesystem or stream operation failed before publication.
    Io {
        /// Sanitized operation label.
        operation: &'static str,
        /// Content-free operating-system error category.
        kind: io::ErrorKind,
    },
}

impl UpdateApplyError {
    fn io(operation: &'static str, error: io::Error) -> Self {
        let kind = error.kind();
        drop(error);
        Self::Io { operation, kind }
    }
}

impl fmt::Display for UpdateApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHost => formatter.write_str("updates are unsupported on this host"),
            Self::ArtifactTargetMismatch => formatter.write_str(
                "release artifact does not match this operating system and architecture",
            ),
            Self::RelativePath => formatter.write_str("update path is not absolute"),
            Self::MissingPathComponent => formatter.write_str("update path is incomplete"),
            Self::UnsafeParentPath => {
                formatter.write_str("update path contains an unsafe directory component")
            }
            Self::UnsafeParentPermissions => {
                formatter.write_str("update installation directory has unsafe write permissions")
            }
            Self::UnsafeAccessControl => {
                formatter.write_str("update path has unsupported extended access controls")
            }
            Self::UnsafeCurrentExecutable => {
                formatter.write_str("current executable is not a safe exclusive regular file")
            }
            Self::UnsafeDownloadedArtifact => {
                formatter.write_str("downloaded artifact is not a safe exclusive regular file")
            }
            Self::EmptyArtifact => formatter.write_str("downloaded artifact is empty"),
            Self::ArtifactTooLarge => {
                formatter.write_str("downloaded artifact exceeds update limit")
            }
            Self::CurrentExecutableTooLarge => {
                formatter.write_str("current executable exceeds update limit")
            }
            Self::ChecksumMismatch => {
                formatter.write_str("downloaded artifact checksum does not match release metadata")
            }
            Self::SourceChanged => formatter.write_str(
                "current executable changed during update and was left untouched; retry update",
            ),
            Self::AtomicReplacementUnavailable => formatter
                .write_str("filesystem does not support safe atomic executable replacement"),
            Self::PublicationUncertain => formatter.write_str(
                "executable pathname changed during publication; ambiguous files were retained",
            ),
            Self::PostPublicationSync { kind } => write!(
                formatter,
                "updated executable was published but directory durability is uncertain: {kind:?}"
            ),
            Self::Io { operation, kind } => write!(formatter, "{operation}: {kind:?}"),
        }
    }
}

impl Error for UpdateApplyError {}

/// Applies downloaded bytes from any bounded stream.
///
/// The explicitly supplied executable path must be absolute. Its complete
/// directory chain and final entry are opened without following symlinks. The
/// downloaded bytes are copied into a private same-directory file, verified,
/// synced, and revalidated before an atomic exchange. No remote byte is ever
/// executed by this function.
///
/// # Errors
///
/// Returns [`UpdateApplyError`] without publishing anything when platform,
/// checksum, size, path, source-identity, or pre-publication filesystem checks
/// fail. [`UpdateApplyError::PostPublicationSync`] is the sole expected error
/// after a verified exchange; see that variant's durability contract. An error
/// after transaction creation may retain a random private transaction entry;
/// this function never deletes a pathname after a separate identity check.
pub fn apply_update_from_reader<R: Read>(
    metadata: &TrustedReleaseArtifact,
    downloaded: &mut R,
    current_executable: &Path,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    apply_update_from_reader_with_hooks(metadata, downloaded, current_executable, || {}, || {})
}

fn apply_update_from_reader_with_hooks<R: Read, B: FnOnce(), C: FnOnce()>(
    metadata: &TrustedReleaseArtifact,
    downloaded: &mut R,
    current_executable: &Path,
    before_exchange: B,
    before_cleanup: C,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    metadata.validate_host_identity()?;
    let target = AnchoredPath::open(current_executable)?;
    let source = CurrentExecutable::open(&target)?;

    target.validate_parent()?;
    let mut transaction = TransactionFile::create(&target)?;
    let artifact_digest = copy_artifact(downloaded, &mut transaction.file)?;
    if artifact_digest != metadata.sha256 {
        return Err(UpdateApplyError::ChecksumMismatch);
    }
    transaction.finish(metadata.sha256)?;

    let source = source.capture(&target)?;
    if source.digest == metadata.sha256 {
        validate_prepublication(&target, &source, &transaction)?;
        target.validate_parent()?;
        if !source.is_current_at(&target, &target.name, true)? {
            return Err(UpdateApplyError::SourceChanged);
        }
        return Ok(UpdateApplyOutcome::AlreadyCurrent);
    }

    validate_prepublication(&target, &source, &transaction)?;
    target
        .parent
        .sync_all()
        .map_err(|error| UpdateApplyError::io("sync update transaction directory", error))?;
    validate_prepublication(&target, &source, &transaction)?;
    transaction.adopt_published_mode(source.snapshot.metadata.mode)?;

    before_exchange();
    validate_prepublication(&target, &source, &transaction)?;
    ensure_no_extended_acl(
        &transaction.file,
        "finalize update transaction access controls",
    )?;
    atomic_exchange(&target, &transaction.name, &target.name)?;
    settle_exchange(&target, &source, &transaction, before_cleanup)
}

/// Applies a downloaded artifact held by an already-open file descriptor.
///
/// The descriptor is required to identify one exclusive regular file and is
/// rewound before reading. Since the descriptor already pins the source inode,
/// this function never reopens or trusts a downloaded-artifact pathname.
///
/// # Errors
///
/// Returns [`UpdateApplyError::UnsafeDownloadedArtifact`] for a non-regular or
/// multiply-linked descriptor, and otherwise forwards errors from
/// [`apply_update_from_reader`].
pub fn apply_update_from_file(
    metadata: &TrustedReleaseArtifact,
    downloaded: &mut File,
    current_executable: &Path,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    validate_downloaded_file(downloaded)?;
    downloaded
        .seek(SeekFrom::Start(0))
        .map_err(|error| UpdateApplyError::io("rewind downloaded update artifact", error))?;
    apply_update_from_reader(metadata, downloaded, current_executable)
}

/// Opens and applies a downloaded artifact through a symlink-free absolute path.
///
/// Prefer [`apply_update_from_file`] when the downloader already owns a pinned
/// file descriptor. This path helper exists for a completed on-disk download
/// and refuses symlinked ancestors, a symlink final entry, hard links, and
/// non-regular files.
///
/// # Errors
///
/// Returns a bounded path, source-type, checksum, or publication error.
pub fn apply_update_from_path(
    metadata: &TrustedReleaseArtifact,
    downloaded_artifact: &Path,
    current_executable: &Path,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    let source_path = AnchoredPath::open(downloaded_artifact)?;
    let mut source = open_named(&source_path.parent, &source_path.name).map_err(|error| {
        if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR) {
            UpdateApplyError::UnsafeDownloadedArtifact
        } else {
            UpdateApplyError::io("open downloaded update artifact", rustix_error(error))
        }
    })?;
    validate_downloaded_file(&source)?;
    if !file_identity_matches_at(&source_path.parent, &source, &source_path.name)? {
        return Err(UpdateApplyError::SourceChanged);
    }
    // The pathname is discarded after this identity check. Its parent need not
    // be private because all downloaded bytes are read from the pinned file
    // descriptor and verified against trusted metadata before publication.
    source_path.validate_parent_identity()?;
    apply_update_from_file(metadata, &mut source, current_executable)
}

fn parse_sha256(value: &str) -> Result<[u8; 32], ReleaseMetadataError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ReleaseMetadataError::InvalidSha256);
    }
    let mut decoded = [0_u8; 32];
    for (destination, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *destination = (decode_lower_hex(pair[0]) << 4) | decode_lower_hex(pair[1]);
    }
    Ok(decoded)
}

const fn decode_lower_hex(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => 0,
    }
}

fn current_operating_system() -> Result<ArtifactOperatingSystem, UpdateApplyError> {
    if cfg!(target_os = "linux") {
        Ok(ArtifactOperatingSystem::Linux)
    } else if cfg!(target_os = "macos") {
        Ok(ArtifactOperatingSystem::Macos)
    } else {
        Err(UpdateApplyError::UnsupportedHost)
    }
}

fn current_architecture() -> Result<ArtifactArchitecture, UpdateApplyError> {
    if cfg!(target_arch = "x86_64") {
        Ok(ArtifactArchitecture::Amd64)
    } else if cfg!(target_arch = "aarch64") {
        Ok(ArtifactArchitecture::Arm64)
    } else {
        Err(UpdateApplyError::UnsupportedHost)
    }
}

struct AnchoredPath {
    parent: File,
    parent_path: PathBuf,
    name: OsString,
}

impl AnchoredPath {
    fn open(path: &Path) -> Result<Self, UpdateApplyError> {
        if !path.is_absolute() {
            return Err(UpdateApplyError::RelativePath);
        }
        let parent_path = platform_anchor_path(required_parent(path)?);
        let name = path
            .file_name()
            .ok_or(UpdateApplyError::MissingPathComponent)?
            .to_os_string();
        let parent = open_directory_chain(&parent_path)?;
        Ok(Self {
            parent,
            parent_path,
            name,
        })
    }

    fn validate_parent(&self) -> Result<(), UpdateApplyError> {
        ensure_safe_install_parent(&self.parent, "inspect update directory")?;
        let reopened = match open_directory_chain(&self.parent_path) {
            Ok(parent) => parent,
            Err(UpdateApplyError::UnsafeParentPath | UpdateApplyError::Io { .. }) => {
                return Err(UpdateApplyError::SourceChanged);
            }
            Err(error) => return Err(error),
        };
        let matches = metadata_identity_matches(
            &self
                .parent
                .metadata()
                .map_err(|error| UpdateApplyError::io("inspect update directory", error))?,
            &reopened
                .metadata()
                .map_err(|error| UpdateApplyError::io("reinspect update directory", error))?,
        );
        ensure_safe_install_parent(&self.parent, "reinspect update directory")?;
        ensure_safe_install_parent(&reopened, "inspect reopened update directory")?;
        if !matches {
            return Err(UpdateApplyError::SourceChanged);
        }
        Ok(())
    }

    fn validate_parent_identity(&self) -> Result<(), UpdateApplyError> {
        let reopened = match open_directory_chain(&self.parent_path) {
            Ok(parent) => parent,
            Err(UpdateApplyError::UnsafeParentPath | UpdateApplyError::Io { .. }) => {
                return Err(UpdateApplyError::SourceChanged);
            }
            Err(error) => return Err(error),
        };
        let anchored = self
            .parent
            .metadata()
            .map_err(|error| UpdateApplyError::io("inspect update directory", error))?;
        let reopened = reopened
            .metadata()
            .map_err(|error| UpdateApplyError::io("reinspect update directory", error))?;
        if !metadata_identity_matches(&anchored, &reopened) {
            return Err(UpdateApplyError::SourceChanged);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn platform_anchor_path(path: &Path) -> PathBuf {
    for (alias, canonical) in [
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/etc"), Path::new("/private/etc")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return canonical.join(suffix);
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "macos"))]
fn platform_anchor_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn required_parent(path: &Path) -> Result<&Path, UpdateApplyError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(UpdateApplyError::MissingPathComponent)
}

fn open_directory_chain(path: &Path) -> Result<File, UpdateApplyError> {
    if !path.is_absolute() {
        return Err(UpdateApplyError::RelativePath);
    }
    let mut directory = File::from(
        rustix::fs::open(
            "/",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(rustix_error)
        .map_err(|error| UpdateApplyError::io("open filesystem root for update", error))?,
    );
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(UpdateApplyError::UnsafeParentPath);
        };
        let opened = File::from(
            rustix::fs::openat(
                &directory,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| {
                if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
                    UpdateApplyError::UnsafeParentPath
                } else {
                    UpdateApplyError::io("open update directory", rustix_error(error))
                }
            })?,
        );
        directory = opened;
    }
    Ok(directory)
}

fn open_named(parent: &File, name: &OsStr) -> Result<File, rustix::io::Errno> {
    rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
}

struct CurrentExecutable {
    file: File,
}

impl CurrentExecutable {
    fn open(target: &AnchoredPath) -> Result<Self, UpdateApplyError> {
        let file = open_named(&target.parent, &target.name).map_err(|error| {
            if matches!(
                error,
                rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR | rustix::io::Errno::MLINK
            ) {
                UpdateApplyError::UnsafeCurrentExecutable
            } else {
                UpdateApplyError::io("open current executable", rustix_error(error))
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| UpdateApplyError::io("inspect current executable", error))?;
        validate_current_metadata(&MetadataSnapshot::from_metadata(&metadata))?;
        ensure_no_extended_acl(&file, "inspect current executable access controls")?;
        target.validate_parent()?;
        if !file_identity_matches_at(&target.parent, &file, &target.name)? {
            return Err(UpdateApplyError::SourceChanged);
        }
        ensure_no_extended_acl(&file, "reinspect current executable access controls")?;
        Ok(Self { file })
    }

    fn capture(self, target: &AnchoredPath) -> Result<CapturedExecutable, UpdateApplyError> {
        let before = FileSnapshot::capture(&self.file, "inspect current executable")?;
        validate_current_metadata(&before.metadata)?;
        ensure_no_extended_acl(&self.file, "inspect current executable access controls")?;
        let digest = digest_file(&self.file, UpdateApplyError::CurrentExecutableTooLarge)?;
        let after = FileSnapshot::capture(&self.file, "reinspect current executable")?;
        if before != after || !file_identity_matches_at(&target.parent, &self.file, &target.name)? {
            return Err(UpdateApplyError::SourceChanged);
        }
        ensure_no_extended_acl(&self.file, "reinspect current executable access controls")?;
        target.validate_parent()?;
        Ok(CapturedExecutable {
            file: self.file,
            snapshot: after,
            digest,
        })
    }
}

struct CapturedExecutable {
    file: File,
    snapshot: FileSnapshot,
    digest: [u8; 32],
}

impl CapturedExecutable {
    fn is_current_at(
        &self,
        target: &AnchoredPath,
        name: &OsStr,
        before_exchange: bool,
    ) -> Result<bool, UpdateApplyError> {
        if !file_identity_matches_at(&target.parent, &self.file, name)? {
            return Ok(false);
        }
        ensure_no_extended_acl(&self.file, "reinspect current executable access controls")?;
        let observed = FileSnapshot::capture(&self.file, "reinspect current executable")?;
        let metadata_matches = if before_exchange {
            observed == self.snapshot
        } else {
            observed.matches_after_rename(&self.snapshot)
        };
        if !metadata_matches {
            return Ok(false);
        }
        let digest = digest_file(&self.file, UpdateApplyError::CurrentExecutableTooLarge)?;
        let after_digest = FileSnapshot::capture(&self.file, "reinspect current executable")?;
        let metadata_still_matches = if before_exchange {
            after_digest == self.snapshot
        } else {
            after_digest.matches_after_rename(&self.snapshot)
        };
        let matches = digest == self.digest
            && metadata_still_matches
            && file_identity_matches_at(&target.parent, &self.file, name)?;
        ensure_no_extended_acl(&self.file, "reinspect current executable access controls")?;
        Ok(matches)
    }
}

#[derive(Eq, PartialEq)]
struct FileSnapshot {
    metadata: MetadataSnapshot,
    changed: ChangeSnapshot,
}

impl FileSnapshot {
    fn capture(file: &File, operation: &'static str) -> Result<Self, UpdateApplyError> {
        let metadata = file
            .metadata()
            .map_err(|error| UpdateApplyError::io(operation, error))?;
        Ok(Self {
            metadata: MetadataSnapshot::from_metadata(&metadata),
            changed: ChangeSnapshot::from_metadata(&metadata),
        })
    }

    fn matches_after_rename(&self, other: &Self) -> bool {
        self.metadata == other.metadata
            && self.changed.modified_seconds == other.changed.modified_seconds
            && self.changed.modified_nanoseconds == other.changed.modified_nanoseconds
    }
}

#[derive(Eq, PartialEq)]
struct MetadataSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
}

impl MetadataSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            owner: metadata.uid(),
            group: metadata.gid(),
            length: metadata.len(),
        }
    }
}

#[derive(Eq, PartialEq)]
struct ChangeSnapshot {
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ChangeSnapshot {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

fn validate_current_metadata(metadata: &MetadataSnapshot) -> Result<(), UpdateApplyError> {
    let file_type = metadata.mode & FILE_TYPE_MASK;
    if file_type != REGULAR_FILE_TYPE
        || metadata.links != 1
        || metadata.mode & 0o111 == 0
        || has_group_or_other_write(metadata.mode)
        || metadata.mode & SET_USER_OR_GROUP_ID_BITS != 0
    {
        return Err(UpdateApplyError::UnsafeCurrentExecutable);
    }
    if metadata.length > MAX_UPDATE_ARTIFACT_BYTES {
        return Err(UpdateApplyError::CurrentExecutableTooLarge);
    }
    Ok(())
}

fn validate_downloaded_file(file: &File) -> Result<(), UpdateApplyError> {
    let metadata = file
        .metadata()
        .map_err(|error| UpdateApplyError::io("inspect downloaded update artifact", error))?;
    validate_downloaded_metadata(&MetadataSnapshot::from_metadata(&metadata))?;
    ensure_no_extended_acl(file, "inspect downloaded artifact access controls")?;
    Ok(())
}

fn validate_downloaded_metadata(metadata: &MetadataSnapshot) -> Result<(), UpdateApplyError> {
    if metadata.mode & FILE_TYPE_MASK != REGULAR_FILE_TYPE
        || metadata.links != 1
        || has_group_or_other_write(metadata.mode)
    {
        return Err(UpdateApplyError::UnsafeDownloadedArtifact);
    }
    if metadata.length > MAX_UPDATE_ARTIFACT_BYTES {
        return Err(UpdateApplyError::ArtifactTooLarge);
    }
    Ok(())
}

struct TransactionFile<'a> {
    parent: &'a File,
    file: File,
    name: OsString,
    snapshot: Option<FileSnapshot>,
    digest: Option<[u8; 32]>,
}

impl<'a> TransactionFile<'a> {
    fn create(target: &'a AnchoredPath) -> Result<Self, UpdateApplyError> {
        target.validate_parent()?;
        let mut random = open_random_source()?;
        for _ in 0..MAX_TEMP_NAME_ATTEMPTS {
            let name = random_transaction_name(&mut random)?;
            match rustix::fs::openat(
                &target.parent,
                &name,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode(0o600),
            ) {
                Ok(file) => {
                    target.validate_parent()?;
                    return Ok(Self {
                        parent: &target.parent,
                        file: File::from(file),
                        name,
                        snapshot: None,
                        digest: None,
                    });
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(UpdateApplyError::io(
                        "create private update transaction",
                        rustix_error(error),
                    ));
                }
            }
        }
        Err(UpdateApplyError::io(
            "create private update transaction",
            io::Error::new(io::ErrorKind::AlreadyExists, "transaction name collisions"),
        ))
    }

    fn finish(&mut self, digest: [u8; 32]) -> Result<(), UpdateApplyError> {
        self.file
            .flush()
            .map_err(|error| UpdateApplyError::io("flush update transaction", error))?;
        self.file
            .set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|error| UpdateApplyError::io("secure update transaction", error))?;
        self.file
            .sync_all()
            .map_err(|error| UpdateApplyError::io("sync update transaction", error))?;
        ensure_no_extended_acl(&self.file, "inspect update transaction access controls")?;
        let snapshot = FileSnapshot::capture(&self.file, "inspect update transaction")?;
        if snapshot.metadata.mode & 0o7777 != PRIVATE_FILE_MODE
            || snapshot.metadata.links != 1
            || snapshot.metadata.length == 0
            || snapshot.metadata.length > MAX_UPDATE_ARTIFACT_BYTES
            || snapshot.metadata.mode & FILE_TYPE_MASK != REGULAR_FILE_TYPE
        {
            return Err(UpdateApplyError::UnsafeDownloadedArtifact);
        }
        self.snapshot = Some(snapshot);
        self.digest = Some(digest);
        if !self.is_current_at(&self.name, true)? {
            return Err(UpdateApplyError::SourceChanged);
        }
        Ok(())
    }

    /// Adopts the mode the published executable must carry.
    ///
    /// The transaction is written privately so a partially written executable is
    /// never runnable by anyone else, but publishing it privately would withdraw
    /// access from every other user of a shared installation. The installed
    /// executable's own mode is restored instead, immediately before the
    /// exchange and ahead of the final validation.
    ///
    /// # Errors
    ///
    /// Rejects a mode that is writable beyond its owner, carries a set-user-ID
    /// or set-group-ID bit, or is not executable by its owner.
    fn adopt_published_mode(&mut self, mode: u32) -> Result<(), UpdateApplyError> {
        let published = mode & 0o7777;
        if published & SET_USER_OR_GROUP_ID_BITS != 0
            || has_group_or_other_write(published)
            || published & 0o100 == 0
        {
            return Err(UpdateApplyError::UnsafeCurrentExecutable);
        }
        self.file
            .set_permissions(std::fs::Permissions::from_mode(published))
            .map_err(|error| UpdateApplyError::io("publish update transaction mode", error))?;
        self.file
            .sync_all()
            .map_err(|error| UpdateApplyError::io("sync published update transaction", error))?;
        ensure_no_extended_acl(&self.file, "republish update transaction access controls")?;
        let snapshot = FileSnapshot::capture(&self.file, "reinspect update transaction")?;
        if snapshot.metadata.mode & 0o7777 != published
            || snapshot.metadata.links != 1
            || snapshot.metadata.length == 0
            || snapshot.metadata.length > MAX_UPDATE_ARTIFACT_BYTES
            || snapshot.metadata.mode & FILE_TYPE_MASK != REGULAR_FILE_TYPE
        {
            return Err(UpdateApplyError::UnsafeDownloadedArtifact);
        }
        self.snapshot = Some(snapshot);
        Ok(())
    }

    fn is_current_at(&self, name: &OsStr, before_exchange: bool) -> Result<bool, UpdateApplyError> {
        if !file_identity_matches_at(self.parent, &self.file, name)? {
            return Ok(false);
        }
        ensure_no_extended_acl(&self.file, "reinspect update transaction access controls")?;
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(false);
        };
        let observed = FileSnapshot::capture(&self.file, "reinspect update transaction")?;
        let metadata_matches = if before_exchange {
            observed == *snapshot
        } else {
            observed.matches_after_rename(snapshot)
        };
        if !metadata_matches {
            return Ok(false);
        }
        let Some(expected_digest) = self.digest else {
            return Ok(false);
        };
        let digest = digest_file(&self.file, UpdateApplyError::ArtifactTooLarge)?;
        let after_digest = FileSnapshot::capture(&self.file, "reinspect update transaction")?;
        let metadata_still_matches = if before_exchange {
            after_digest == *snapshot
        } else {
            after_digest.matches_after_rename(snapshot)
        };
        let matches = digest == expected_digest
            && metadata_still_matches
            && file_identity_matches_at(self.parent, &self.file, name)?;
        ensure_no_extended_acl(&self.file, "reinspect update transaction access controls")?;
        Ok(matches)
    }
}

fn open_random_source() -> Result<File, UpdateApplyError> {
    let random = File::from(
        rustix::fs::open(
            "/dev/urandom",
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(rustix_error)
        .map_err(|error| UpdateApplyError::io("open operating-system random source", error))?,
    );
    let metadata = random
        .metadata()
        .map_err(|error| UpdateApplyError::io("inspect operating-system random source", error))?;
    if !metadata.file_type().is_char_device() {
        return Err(UpdateApplyError::io(
            "inspect operating-system random source",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "random source is not a character device",
            ),
        ));
    }
    Ok(random)
}

fn random_transaction_name(random: &mut File) -> Result<OsString, UpdateApplyError> {
    let mut bytes = [0_u8; RANDOM_NAME_BYTES];
    random
        .read_exact(&mut bytes)
        .map_err(|error| UpdateApplyError::io("read operating-system random source", error))?;
    let mut name = String::with_capacity(".argmax-update-.tmp".len() + RANDOM_NAME_BYTES * 2);
    name.push_str(".argmax-update-");
    for byte in bytes {
        use fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    name.push_str(".tmp");
    Ok(name.into())
}

fn copy_artifact<R: Read>(
    downloaded: &mut R,
    transaction: &mut File,
) -> Result<[u8; 32], UpdateApplyError> {
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    loop {
        let count = downloaded
            .read(&mut buffer)
            .map_err(|error| UpdateApplyError::io("read downloaded update artifact", error))?;
        if count == 0 {
            break;
        }
        if count > buffer.len() {
            return Err(UpdateApplyError::io(
                "read downloaded update artifact",
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reader returned more bytes than its buffer",
                ),
            ));
        }
        total = total
            .checked_add(u64::try_from(count).expect("copy buffer length fits u64"))
            .ok_or(UpdateApplyError::ArtifactTooLarge)?;
        if total > MAX_UPDATE_ARTIFACT_BYTES {
            return Err(UpdateApplyError::ArtifactTooLarge);
        }
        transaction
            .write_all(&buffer[..count])
            .map_err(|error| UpdateApplyError::io("write update transaction", error))?;
        digest.update(&buffer[..count]);
    }
    if total == 0 {
        return Err(UpdateApplyError::EmptyArtifact);
    }
    Ok(digest.finalize().into())
}

fn digest_file(file: &File, too_large: UpdateApplyError) -> Result<[u8; 32], UpdateApplyError> {
    let mut file = file
        .try_clone()
        .map_err(|error| UpdateApplyError::io("clone executable for verification", error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| UpdateApplyError::io("rewind executable for verification", error))?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice();
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| UpdateApplyError::io("read executable for verification", error))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).expect("verification buffer length fits u64"))
            .ok_or(too_large)?;
        if total > MAX_UPDATE_ARTIFACT_BYTES {
            return Err(too_large);
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

fn validate_prepublication(
    target: &AnchoredPath,
    source: &CapturedExecutable,
    transaction: &TransactionFile<'_>,
) -> Result<(), UpdateApplyError> {
    target.validate_parent()?;
    if !source.is_current_at(target, &target.name, true)?
        || !transaction.is_current_at(&transaction.name, true)?
    {
        return Err(UpdateApplyError::SourceChanged);
    }
    target.validate_parent()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn atomic_exchange(
    target: &AnchoredPath,
    left: &OsStr,
    right: &OsStr,
) -> Result<(), UpdateApplyError> {
    rustix::fs::renameat_with(
        &target.parent,
        left,
        &target.parent,
        right,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::NOENT {
            return UpdateApplyError::SourceChanged;
        }
        if matches!(
            error,
            rustix::io::Errno::NOSYS
                | rustix::io::Errno::NOTSUP
                | rustix::io::Errno::INVAL
                | rustix::io::Errno::XDEV
        ) {
            UpdateApplyError::AtomicReplacementUnavailable
        } else {
            UpdateApplyError::io("atomically exchange executable", rustix_error(error))
        }
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn atomic_exchange(
    _target: &AnchoredPath,
    _left: &OsStr,
    _right: &OsStr,
) -> Result<(), UpdateApplyError> {
    Err(UpdateApplyError::UnsupportedHost)
}

fn settle_exchange<C: FnOnce()>(
    target: &AnchoredPath,
    source: &CapturedExecutable,
    transaction: &TransactionFile<'_>,
    before_cleanup: C,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    let source_at_transaction = source
        .is_current_at(target, &transaction.name, false)
        .unwrap_or(false);
    let transaction_at_target = transaction
        .is_current_at(&target.name, false)
        .unwrap_or(false);
    let parent_is_current = target.validate_parent().is_ok();

    if !(source_at_transaction && transaction_at_target && parent_is_current) {
        return Err(UpdateApplyError::PublicationUncertain);
    }

    if let Err(error) = target.parent.sync_all() {
        return Err(UpdateApplyError::PostPublicationSync { kind: error.kind() });
    }
    if target.validate_parent().is_err()
        || !source
            .is_current_at(target, &transaction.name, false)
            .unwrap_or(false)
        || !transaction
            .is_current_at(&target.name, false)
            .unwrap_or(false)
    {
        return Err(UpdateApplyError::PublicationUncertain);
    }

    before_cleanup();
    Ok(UpdateApplyOutcome::Updated {
        cleanup: PreviousExecutableCleanup::Retained,
    })
}

fn file_identity_matches_at(
    parent: &File,
    anchored: &File,
    name: &OsStr,
) -> Result<bool, UpdateApplyError> {
    ensure_no_extended_acl(anchored, "inspect anchored update file access controls")?;
    let anchored = anchored
        .metadata()
        .map_err(|error| UpdateApplyError::io("inspect anchored update file", error))?;
    let named = match open_named(parent, name) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => return Ok(false),
        Err(error) => {
            return Err(UpdateApplyError::io(
                "reopen anchored update file",
                rustix_error(error),
            ));
        }
    };
    let named_metadata = named
        .metadata()
        .map_err(|error| UpdateApplyError::io("reinspect anchored update file", error))?;
    let matches = metadata_identity_matches(&anchored, &named_metadata) && named_metadata.is_file();
    if matches {
        ensure_no_extended_acl(&named, "reinspect anchored update file access controls")?;
    }
    Ok(matches)
}

fn ensure_safe_install_parent(
    directory: &File,
    operation: &'static str,
) -> Result<(), UpdateApplyError> {
    let metadata = directory
        .metadata()
        .map_err(|error| UpdateApplyError::io(operation, error))?;
    if !metadata.is_dir() || has_group_or_other_write(metadata.mode()) {
        return Err(UpdateApplyError::UnsafeParentPermissions);
    }
    // Clean write bits are not enough: the directory's own owner may rename
    // entries between validation and publication regardless of mode. Only
    // root and the invoking user are trusted, matching the ownership rule
    // the shell installer applies to its own destination.
    if !is_trusted_owner(metadata.uid()) {
        return Err(UpdateApplyError::UnsafeParentPermissions);
    }
    ensure_no_extended_acl(directory, operation)
}

fn is_trusted_owner(owner: u32) -> bool {
    owner == 0 || owner == rustix::process::geteuid().as_raw()
}

const fn has_group_or_other_write(mode: u32) -> bool {
    mode & GROUP_OR_OTHER_WRITE_BITS != 0
}

#[cfg(target_os = "macos")]
fn ensure_no_extended_acl(file: &File, operation: &'static str) -> Result<(), UpdateApplyError> {
    if argmax_platform::macos::has_extended_acl(file)
        .map_err(|error| UpdateApplyError::io(operation, error))?
    {
        return Err(UpdateApplyError::UnsafeAccessControl);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)]
fn ensure_no_extended_acl(_file: &File, _operation: &'static str) -> Result<(), UpdateApplyError> {
    Ok(())
}

fn metadata_identity_matches(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn rustix_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Cursor;
    use std::os::unix::fs::{MetadataExt as _, symlink};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;

    #[cfg(target_os = "macos")]
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    const OLD_BYTES: &[u8] = b"old argmax executable\n";
    const NEW_BYTES: &[u8] = b"new verified argmax executable\n";

    fn current_identity() -> (&'static str, &'static str) {
        let operating_system = if cfg!(target_os = "linux") {
            "linux"
        } else {
            "macos"
        };
        let architecture = if cfg!(target_arch = "x86_64") {
            "amd64"
        } else {
            "arm64"
        };
        (operating_system, architecture)
    }

    fn checksum(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        digest
            .iter()
            .fold(String::with_capacity(64), |mut checksum, byte| {
                use fmt::Write as _;
                write!(&mut checksum, "{byte:02x}").unwrap();
                checksum
            })
    }

    fn metadata_for(bytes: &[u8]) -> TrustedReleaseArtifact {
        let (operating_system, architecture) = current_identity();
        TrustedReleaseArtifact::new(
            "1.2.3-nightly.4+build.5",
            operating_system,
            architecture,
            &checksum(bytes),
        )
        .unwrap()
    }

    fn executable(directory: &Path) -> PathBuf {
        let path = directory.join("argmax");
        fs::write(&path, OLD_BYTES).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn transaction_entries(directory: &Path) -> Vec<OsString> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".argmax-update-"))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn add_macos_acl(path: &Path, rule: &str) {
        let status = Command::new("/bin/chmod")
            .args(["+a", rule])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(argmax_platform::macos::has_extended_acl(File::open(path).unwrap()).unwrap());
    }

    #[cfg(target_os = "macos")]
    fn has_macos_acl(path: &Path) -> bool {
        argmax_platform::macos::has_extended_acl(File::open(path).unwrap()).unwrap()
    }

    #[cfg(target_os = "macos")]
    fn remove_macos_acl(path: &Path) {
        let status = Command::new("/bin/chmod")
            .arg("-N")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!has_macos_acl(path));
    }

    #[test]
    fn metadata_is_strict_and_debug_redacts_values() {
        let (operating_system, architecture) = current_identity();
        assert!(matches!(
            TrustedReleaseArtifact::new("v1.2.3", operating_system, architecture, &"a".repeat(64)),
            Err(ReleaseMetadataError::InvalidVersion(_))
        ));
        assert_eq!(
            TrustedReleaseArtifact::new("1.2.3", "darwin", architecture, &"a".repeat(64))
                .unwrap_err(),
            ReleaseMetadataError::InvalidOperatingSystem
        );
        assert_eq!(
            TrustedReleaseArtifact::new("1.2.3", operating_system, "x64", &"a".repeat(64))
                .unwrap_err(),
            ReleaseMetadataError::InvalidArchitecture
        );
        assert_eq!(
            TrustedReleaseArtifact::new("1.2.3", operating_system, architecture, &"A".repeat(64))
                .unwrap_err(),
            ReleaseMetadataError::InvalidSha256
        );
        let metadata = metadata_for(NEW_BYTES);
        let debug = format!("{metadata:?}");
        assert!(!debug.contains("1.2.3"));
        assert!(!debug.contains(&checksum(NEW_BYTES)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn deny_only_acl_on_ancestor_does_not_block_update() {
        let root = TempDir::new().unwrap();
        let ancestor = root.path().join("acl-ancestor");
        let install = ancestor.join("install");
        fs::create_dir_all(&install).unwrap();
        let executable = executable(&install);
        add_macos_acl(&ancestor, "everyone deny delete");
        assert!(!has_macos_acl(&install));
        let mut artifact = Cursor::new(NEW_BYTES);

        let result = apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable);
        remove_macos_acl(&ancestor);

        assert_eq!(
            result.unwrap(),
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inheriting_everyone_acl_blocks_update_before_transaction_creation() {
        let root = TempDir::new().unwrap();
        let install = root.path().join("install");
        fs::create_dir(&install).unwrap();
        let executable = executable(&install);
        add_macos_acl(
            &install,
            "everyone allow write,execute,file_inherit,directory_inherit",
        );
        let mut artifact = Cursor::new(NEW_BYTES);

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeAccessControl
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert!(!has_macos_acl(&executable));
        assert!(transaction_entries(&install).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parent_only_mutation_acl_blocks_before_transaction_creation() {
        let root = TempDir::new().unwrap();
        let install = root.path().join("install");
        fs::create_dir(&install).unwrap();
        let executable = executable(&install);
        add_macos_acl(&install, "everyone allow add_file,delete_child");
        let mut artifact = Cursor::new(NEW_BYTES);

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeAccessControl
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert!(transaction_entries(&install).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn current_executable_acl_blocks_before_transaction_creation() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        add_macos_acl(&executable, "everyone allow read");
        let mut artifact = Cursor::new(NEW_BYTES);

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeAccessControl
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert!(has_macos_acl(&executable));
        assert!(transaction_entries(directory.path()).is_empty());
    }

    #[test]
    fn update_publishes_the_mode_the_installation_already_had() {
        for mode in [0o755, 0o700, 0o750, 0o555] {
            let directory = TempDir::new().unwrap();
            let executable = executable(directory.path());
            fs::set_permissions(&executable, fs::Permissions::from_mode(mode)).unwrap();
            let mut artifact = Cursor::new(NEW_BYTES);

            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable).unwrap();

            assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
            assert_eq!(
                fs::metadata(&executable).unwrap().mode() & 0o7777,
                mode,
                "update did not preserve mode {mode:o}"
            );
        }
    }

    #[test]
    fn verified_update_is_atomic_private_and_retains_prior_executable() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let mut artifact = Cursor::new(NEW_BYTES);

        let outcome =
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable).unwrap();

        assert_eq!(
            outcome,
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
        assert_eq!(fs::metadata(&executable).unwrap().mode() & 0o7777, 0o755);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            OLD_BYTES
        );
    }

    #[test]
    fn checksum_failure_preserves_current_executable() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let original_inode = fs::metadata(&executable).unwrap().ino();
        let mut artifact = Cursor::new(b"tampered artifact");

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::ChecksumMismatch
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert_eq!(fs::metadata(&executable).unwrap().ino(), original_inode);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            b"tampered artifact"
        );
    }

    struct RepeatingReader {
        remaining: u64,
    }

    impl Read for RepeatingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let count = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap();
            buffer[..count].fill(b'x');
            self.remaining -= u64::try_from(count).unwrap();
            Ok(count)
        }
    }

    #[test]
    fn oversized_stream_and_current_file_fail_closed() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let mut artifact = RepeatingReader {
            remaining: MAX_UPDATE_ARTIFACT_BYTES + 1,
        };
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::ArtifactTooLarge
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);

        OpenOptions::new()
            .write(true)
            .open(&executable)
            .unwrap()
            .set_len(MAX_UPDATE_ARTIFACT_BYTES + 1)
            .unwrap();
        let mut small = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut small, &executable)
                .unwrap_err(),
            UpdateApplyError::CurrentExecutableTooLarge
        );
        assert_eq!(
            fs::metadata(&executable).unwrap().len(),
            MAX_UPDATE_ARTIFACT_BYTES + 1
        );
        assert_eq!(transaction_entries(directory.path()).len(), 1);
    }

    #[test]
    fn symlink_and_nonregular_targets_are_rejected() {
        let directory = TempDir::new().unwrap();
        let real = executable(directory.path());
        let linked = directory.path().join("linked-argmax");
        symlink(&real, &linked).unwrap();
        let mut artifact = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &linked).unwrap_err(),
            UpdateApplyError::UnsafeCurrentExecutable
        );

        let nonregular = directory.path().join("directory-target");
        fs::create_dir(&nonregular).unwrap();
        let mut artifact = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &nonregular)
                .unwrap_err(),
            UpdateApplyError::UnsafeCurrentExecutable
        );
        assert_eq!(fs::read(real).unwrap(), OLD_BYTES);
    }

    #[test]
    fn unsafe_target_permissions_and_hard_links_are_rejected() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
        let mut artifact = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeCurrentExecutable
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let hard_link = directory.path().join("argmax-hard-link");
        fs::hard_link(&executable, &hard_link).unwrap();
        let mut artifact = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeCurrentExecutable
        );
        assert_eq!(fs::read(&hard_link).unwrap(), OLD_BYTES);
        assert!(transaction_entries(directory.path()).is_empty());
    }

    #[test]
    fn group_or_other_writable_install_parent_is_rejected() {
        for (name, mode) in [("group-writable", 0o770), ("other-writable", 0o707)] {
            let root = TempDir::new().unwrap();
            let install = root.path().join(name);
            fs::create_dir(&install).unwrap();
            let executable = executable(&install);
            fs::set_permissions(&install, fs::Permissions::from_mode(mode)).unwrap();
            let mut artifact = Cursor::new(NEW_BYTES);

            assert_eq!(
                apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                    .unwrap_err(),
                UpdateApplyError::UnsafeParentPermissions
            );
            assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
            assert!(transaction_entries(&install).is_empty());
        }
    }

    #[test]
    fn only_root_and_the_current_user_own_a_trusted_install_parent() {
        let current = rustix::process::geteuid().as_raw();
        assert!(is_trusted_owner(0));
        assert!(is_trusted_owner(current));

        // A directory owned by a third party may be rewritten by that owner
        // between validation and publication regardless of its mode.
        let foreign = (1..=u32::MAX)
            .find(|uid| *uid != current && *uid != 0)
            .unwrap();
        assert!(!is_trusted_owner(foreign));
    }

    #[test]
    fn group_or_other_writable_current_executable_is_rejected() {
        for (name, mode) in [("group-writable", 0o775), ("other-writable", 0o757)] {
            let root = TempDir::new().unwrap();
            let install = root.path().join(name);
            fs::create_dir(&install).unwrap();
            let executable = executable(&install);
            fs::set_permissions(&executable, fs::Permissions::from_mode(mode)).unwrap();
            let mut artifact = Cursor::new(NEW_BYTES);

            assert_eq!(
                apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                    .unwrap_err(),
                UpdateApplyError::UnsafeCurrentExecutable
            );
            assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
            assert!(transaction_entries(&install).is_empty());
        }
    }

    #[test]
    fn set_user_or_group_id_current_executable_is_rejected() {
        let root = TempDir::new().unwrap();
        let executable = executable(root.path());
        let base = fs::metadata(executable).unwrap();
        for mode in [0o4755, 0o2755] {
            let mut metadata = MetadataSnapshot::from_metadata(&base);
            metadata.mode = (metadata.mode & !0o7777) | mode;
            assert_eq!(metadata.mode & 0o7777, mode);
            assert_eq!(
                validate_current_metadata(&metadata).unwrap_err(),
                UpdateApplyError::UnsafeCurrentExecutable
            );
        }
    }

    #[test]
    fn nonwritable_parent_special_bits_are_not_rejected() {
        assert!(!has_group_or_other_write(0o2755));
        assert!(!has_group_or_other_write(0o1755));
        let root = TempDir::new().unwrap();
        let install = root.path().join("install");
        fs::create_dir(&install).unwrap();
        fs::set_permissions(&install, fs::Permissions::from_mode(0o1755)).unwrap();
        assert_eq!(fs::metadata(&install).unwrap().mode() & 0o1022, 0o1000);
        let executable = executable(&install);
        let mut artifact = Cursor::new(NEW_BYTES);

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable).unwrap(),
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
        assert_eq!(fs::metadata(&executable).unwrap().mode() & 0o7777, 0o755);
    }

    #[test]
    fn downloaded_path_rejects_symlinks_and_nonregular_files() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let artifact = directory.path().join("artifact");
        fs::write(&artifact, NEW_BYTES).unwrap();
        let linked = directory.path().join("artifact-link");
        symlink(&artifact, &linked).unwrap();
        assert_eq!(
            apply_update_from_path(&metadata_for(NEW_BYTES), &linked, &executable).unwrap_err(),
            UpdateApplyError::UnsafeDownloadedArtifact
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);

        let directory_artifact = directory.path().join("artifact-directory");
        fs::create_dir(&directory_artifact).unwrap();
        assert_eq!(
            apply_update_from_path(&metadata_for(NEW_BYTES), &directory_artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeDownloadedArtifact
        );
    }

    #[test]
    fn downloaded_file_is_rewound_and_verified_from_its_descriptor() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let artifact_path = directory.path().join("artifact-file");
        fs::write(&artifact_path, NEW_BYTES).unwrap();
        let mut artifact = OpenOptions::new().read(true).open(&artifact_path).unwrap();
        artifact.seek(SeekFrom::End(0)).unwrap();

        assert_eq!(
            apply_update_from_file(&metadata_for(NEW_BYTES), &mut artifact, &executable).unwrap(),
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(executable).unwrap(), NEW_BYTES);
        assert_eq!(fs::read(artifact_path).unwrap(), NEW_BYTES);
        assert_eq!(transaction_entries(directory.path()).len(), 1);
    }

    #[test]
    fn group_or_other_writable_downloaded_file_is_rejected() {
        for (name, mode) in [("group-writable", 0o660), ("other-writable", 0o606)] {
            let root = TempDir::new().unwrap();
            let install = root.path().join(name);
            fs::create_dir(&install).unwrap();
            let executable = executable(&install);
            let artifact_path = root.path().join(format!("{name}-artifact"));
            fs::write(&artifact_path, NEW_BYTES).unwrap();
            fs::set_permissions(&artifact_path, fs::Permissions::from_mode(mode)).unwrap();
            let mut artifact = File::open(&artifact_path).unwrap();

            assert_eq!(
                apply_update_from_file(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                    .unwrap_err(),
                UpdateApplyError::UnsafeDownloadedArtifact
            );
            assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
            assert!(transaction_entries(&install).is_empty());
        }
    }

    #[test]
    fn private_download_in_writable_parent_is_verified_from_pinned_descriptor() {
        let root = TempDir::new().unwrap();
        let install = root.path().join("install");
        let downloads = root.path().join("downloads");
        fs::create_dir(&install).unwrap();
        fs::create_dir(&downloads).unwrap();
        fs::set_permissions(&downloads, fs::Permissions::from_mode(0o777)).unwrap();
        let executable = executable(&install);
        let artifact = downloads.join("artifact");
        fs::write(&artifact, NEW_BYTES).unwrap();
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(
            apply_update_from_path(&metadata_for(NEW_BYTES), &artifact, &executable).unwrap(),
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
        assert_eq!(fs::read(&artifact).unwrap(), NEW_BYTES);
    }

    #[test]
    fn downloaded_special_permission_bits_are_not_propagated() {
        let root = TempDir::new().unwrap();
        let artifact_path = root.path().join("privileged-artifact");
        fs::write(&artifact_path, NEW_BYTES).unwrap();
        let mut metadata = MetadataSnapshot::from_metadata(&fs::metadata(artifact_path).unwrap());
        metadata.mode = (metadata.mode & !0o7777) | 0o6700;

        assert!(validate_downloaded_metadata(&metadata).is_ok());
        assert_eq!(PRIVATE_FILE_MODE & SET_USER_OR_GROUP_ID_BITS, 0);
    }

    #[test]
    fn same_artifact_is_idempotent_without_replacing_inode() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let metadata = metadata_for(NEW_BYTES);
        let mut first = Cursor::new(NEW_BYTES);
        apply_update_from_reader(&metadata, &mut first, &executable).unwrap();
        let inode = fs::metadata(&executable).unwrap().ino();

        let mut second = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata, &mut second, &executable).unwrap(),
            UpdateApplyOutcome::AlreadyCurrent
        );
        assert_eq!(fs::metadata(&executable).unwrap().ino(), inode);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 2);
        let mut retained = transactions
            .iter()
            .map(|name| fs::read(directory.path().join(name)).unwrap())
            .collect::<Vec<_>>();
        retained.sort();
        let mut want = vec![OLD_BYTES.to_vec(), NEW_BYTES.to_vec()];
        want.sort();
        assert_eq!(retained, want);
    }

    #[test]
    fn path_readers_observe_only_complete_old_or_new_executables() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let (opened_tx, opened_rx) = mpsc::channel();
        let (proceed_tx, proceed_rx) = mpsc::channel();
        let thread_executable = executable.clone();
        let updater = thread::spawn(move || {
            let mut reader = BlockingReader {
                bytes: Cursor::new(NEW_BYTES),
                opened: Some(opened_tx),
                proceed: proceed_rx,
            };
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut reader, &thread_executable)
        });

        opened_rx.recv().unwrap();
        let observing = Arc::new(AtomicBool::new(true));
        let observer_flag = Arc::clone(&observing);
        let observer_path = executable.clone();
        let observer = thread::spawn(move || {
            while observer_flag.load(Ordering::Acquire) {
                let bytes = fs::read(&observer_path).unwrap();
                assert!(bytes == OLD_BYTES || bytes == NEW_BYTES);
            }
        });
        proceed_tx.send(()).unwrap();
        assert!(matches!(
            updater.join().unwrap().unwrap(),
            UpdateApplyOutcome::Updated { .. }
        ));
        observing.store(false, Ordering::Release);
        observer.join().unwrap();
        assert_eq!(fs::read(executable).unwrap(), NEW_BYTES);
    }

    #[test]
    fn wrong_platform_identity_fails_before_filesystem_changes() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let (operating_system, architecture) = current_identity();
        let wrong_operating_system = if operating_system == "linux" {
            "macos"
        } else {
            "linux"
        };
        let metadata = TrustedReleaseArtifact::new(
            "1.2.3",
            wrong_operating_system,
            architecture,
            &checksum(NEW_BYTES),
        )
        .unwrap();
        let mut artifact = Cursor::new(NEW_BYTES);
        assert_eq!(
            apply_update_from_reader(&metadata, &mut artifact, &executable).unwrap_err(),
            UpdateApplyError::ArtifactTargetMismatch
        );
        assert_eq!(fs::read(executable).unwrap(), OLD_BYTES);
        assert!(transaction_entries(directory.path()).is_empty());
    }

    struct FailingReader {
        first: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.first {
                self.first = false;
                buffer[..4].copy_from_slice(b"part");
                Ok(4)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "secret remote failure",
                ))
            }
        }
    }

    #[test]
    fn stream_failure_preserves_target_and_redacts_source_error() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let mut artifact = FailingReader { first: true };
        let error = apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
            .unwrap_err();
        assert_eq!(
            error,
            UpdateApplyError::Io {
                operation: "read downloaded update artifact",
                kind: io::ErrorKind::ConnectionReset,
            }
        );
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(fs::read(executable).unwrap(), OLD_BYTES);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            b"part"
        );
    }

    #[cfg(target_os = "macos")]
    struct TransactionAclReader {
        bytes: Cursor<&'static [u8]>,
        directory: PathBuf,
        injected: bool,
    }

    #[cfg(target_os = "macos")]
    impl Read for TransactionAclReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.injected {
                let transactions = transaction_entries(&self.directory);
                assert_eq!(transactions.len(), 1);
                add_macos_acl(
                    &self.directory.join(&transactions[0]),
                    "everyone allow write,execute",
                );
                self.injected = true;
            }
            self.bytes.read(buffer)
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn transaction_acl_is_rejected_after_chmod_and_sync() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let mut artifact = TransactionAclReader {
            bytes: Cursor::new(NEW_BYTES),
            directory: directory.path().to_path_buf(),
            injected: false,
        };

        assert_eq!(
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut artifact, &executable)
                .unwrap_err(),
            UpdateApplyError::UnsafeAccessControl
        );
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert!(!has_macos_acl(&executable));
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert!(has_macos_acl(&directory.path().join(&transactions[0])));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn transaction_acl_is_revalidated_at_publication_boundary() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let mut artifact = Cursor::new(NEW_BYTES);

        let error = apply_update_from_reader_with_hooks(
            &metadata_for(NEW_BYTES),
            &mut artifact,
            &executable,
            || {
                let transactions = transaction_entries(directory.path());
                assert_eq!(transactions.len(), 1);
                add_macos_acl(
                    &directory.path().join(&transactions[0]),
                    "everyone allow write,execute",
                );
            },
            || {},
        )
        .unwrap_err();

        assert_eq!(error, UpdateApplyError::UnsafeAccessControl);
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert!(!has_macos_acl(&executable));
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert!(has_macos_acl(&directory.path().join(&transactions[0])));
    }

    struct BlockingReader {
        bytes: Cursor<&'static [u8]>,
        opened: Option<mpsc::Sender<()>>,
        proceed: mpsc::Receiver<()>,
    }

    impl Read for BlockingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if let Some(opened) = self.opened.take() {
                opened.send(()).unwrap();
                self.proceed.recv().unwrap();
            }
            self.bytes.read(buffer)
        }
    }

    #[test]
    fn target_substitution_during_download_preserves_foreign_entry() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let moved = directory.path().join("moved-original");
        let (opened_tx, opened_rx) = mpsc::channel();
        let (proceed_tx, proceed_rx) = mpsc::channel();
        let thread_executable = executable.clone();
        let worker = thread::spawn(move || {
            let mut reader = BlockingReader {
                bytes: Cursor::new(NEW_BYTES),
                opened: Some(opened_tx),
                proceed: proceed_rx,
            };
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut reader, &thread_executable)
        });

        opened_rx.recv().unwrap();
        fs::rename(&executable, &moved).unwrap();
        fs::write(&executable, b"foreign executable\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        proceed_tx.send(()).unwrap();

        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            UpdateApplyError::SourceChanged
        );
        assert_eq!(fs::read(&executable).unwrap(), b"foreign executable\n");
        assert_eq!(fs::read(&moved).unwrap(), OLD_BYTES);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            NEW_BYTES
        );
    }

    #[test]
    fn target_substitution_at_final_boundary_is_revalidated() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let moved_original = directory.path().join("moved-original");
        let mut artifact = Cursor::new(NEW_BYTES);

        let error = apply_update_from_reader_with_hooks(
            &metadata_for(NEW_BYTES),
            &mut artifact,
            &executable,
            || {
                fs::rename(&executable, &moved_original).unwrap();
                fs::write(&executable, b"foreign executable\n").unwrap();
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            },
            || {},
        )
        .unwrap_err();

        assert_eq!(error, UpdateApplyError::SourceChanged);
        assert_eq!(fs::read(&executable).unwrap(), b"foreign executable\n");
        assert_eq!(fs::read(&moved_original).unwrap(), OLD_BYTES);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            NEW_BYTES
        );
    }

    #[test]
    fn transaction_substitution_at_final_boundary_is_revalidated() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let moved_artifact = directory.path().join("moved-artifact");
        let mut artifact = Cursor::new(NEW_BYTES);

        let error = apply_update_from_reader_with_hooks(
            &metadata_for(NEW_BYTES),
            &mut artifact,
            &executable,
            || {
                let transactions = transaction_entries(directory.path());
                assert_eq!(transactions.len(), 1);
                let transaction = directory.path().join(&transactions[0]);
                fs::rename(&transaction, &moved_artifact).unwrap();
                fs::write(&transaction, b"foreign transaction\n").unwrap();
            },
            || {},
        )
        .unwrap_err();

        assert_eq!(error, UpdateApplyError::SourceChanged);
        assert_eq!(fs::read(&executable).unwrap(), OLD_BYTES);
        assert_eq!(fs::read(&moved_artifact).unwrap(), NEW_BYTES);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            b"foreign transaction\n"
        );
    }

    #[test]
    fn cleanup_substitution_retains_unrelated_file() {
        let directory = TempDir::new().unwrap();
        let executable = executable(directory.path());
        let moved_previous = directory.path().join("moved-previous");
        let mut artifact = Cursor::new(NEW_BYTES);

        let outcome = apply_update_from_reader_with_hooks(
            &metadata_for(NEW_BYTES),
            &mut artifact,
            &executable,
            || {},
            || {
                let transactions = transaction_entries(directory.path());
                assert_eq!(transactions.len(), 1);
                let transaction = directory.path().join(&transactions[0]);
                fs::rename(&transaction, &moved_previous).unwrap();
                fs::write(&transaction, b"unrelated file\n").unwrap();
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            UpdateApplyOutcome::Updated {
                cleanup: PreviousExecutableCleanup::Retained
            }
        );
        assert_eq!(fs::read(&executable).unwrap(), NEW_BYTES);
        assert_eq!(fs::read(&moved_previous).unwrap(), OLD_BYTES);
        let transactions = transaction_entries(directory.path());
        assert_eq!(transactions.len(), 1);
        assert_eq!(
            fs::read(directory.path().join(&transactions[0])).unwrap(),
            b"unrelated file\n"
        );
    }

    #[test]
    fn parent_rebind_during_download_updates_neither_directory() {
        let root = TempDir::new().unwrap();
        let install = root.path().join("install");
        let moved = root.path().join("moved-install");
        fs::create_dir(&install).unwrap();
        let executable_path = executable(&install);
        let (opened_tx, opened_rx) = mpsc::channel();
        let (proceed_tx, proceed_rx) = mpsc::channel();
        let thread_executable = executable_path.clone();
        let worker = thread::spawn(move || {
            let mut reader = BlockingReader {
                bytes: Cursor::new(NEW_BYTES),
                opened: Some(opened_tx),
                proceed: proceed_rx,
            };
            apply_update_from_reader(&metadata_for(NEW_BYTES), &mut reader, &thread_executable)
        });

        opened_rx.recv().unwrap();
        fs::rename(&install, &moved).unwrap();
        fs::create_dir(&install).unwrap();
        let rebound = executable(&install);
        fs::write(&rebound, b"foreign executable\n").unwrap();
        fs::set_permissions(&rebound, fs::Permissions::from_mode(0o700)).unwrap();
        proceed_tx.send(()).unwrap();

        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            UpdateApplyError::SourceChanged
        );
        assert_eq!(fs::read(&rebound).unwrap(), b"foreign executable\n");
        assert_eq!(fs::read(moved.join("argmax")).unwrap(), OLD_BYTES);
        let transactions = transaction_entries(&moved);
        assert_eq!(transactions.len(), 1);
        assert_eq!(fs::read(moved.join(&transactions[0])).unwrap(), NEW_BYTES);
        assert!(transaction_entries(&install).is_empty());
    }
}
